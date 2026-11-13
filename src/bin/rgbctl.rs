//! `rgbctl` — hardware probe and lighting test tool for the AULA F99.
//!
//! Read-only probes plus explicit AULA F99 and Razer Viper Mini controls.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use rgbd::color::Rgb;
use rgbd::hid::{HidManager, usage_page_name};
use rgbd::razer::{self, Device as RazerDevice, class as razer_class, cmd as razer_cmd};
use rgbd::sinowealth::{
    AULA_F99_MODEL_ID, CONFIG_LEN, CONFIG_MAGIC, CONFIG_SETTLE_MS, KEEPALIVE_MS, Keyboard, MAX_FPS,
    MIN_FRAME_GAP_MS, MODE_PER_KEY, STREAM_SLOTS, config_changes, is_per_key_mode, is_valid_config,
    mode_of, model_name, parse_led_map, with_mode,
};

#[derive(Parser, Debug)]
#[command(
    name = "rgbctl",
    version,
    about = "Probe and control AULA F99 and Razer Viper Mini over their HID protocols"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Enumerate HID collections and confirm the RGB protocol (read-only).
    Probe,

    /// Enumerate the Razer Viper Mini and read firmware, serial, mode, DPI,
    /// polling rate, DPI stages, and logo brightness (read-only).
    RazerProbe {
        /// Also print the raw response for every read command.
        #[arg(long)]
        raw: bool,
    },

    /// Change a Viper Mini setting with guarded writes and readback.
    RazerSet {
        #[command(subcommand)]
        setting: RazerSetting,
    },

    /// Read the 128-byte onboard config and save a backup (read-only).
    Config {
        /// Directory for the backup file. Defaults to %APPDATA%\rgbd\backups.
        #[arg(long)]
        backup_dir: Option<PathBuf>,
    },

    /// Read the keyboard's LED map (command 0x83, read-only).
    LedMap {
        /// Directory for the raw map file. Defaults to %APPDATA%\rgbd\backups.
        #[arg(long)]
        backup_dir: Option<PathBuf>,
    },

    /// Compare two config backups byte by byte (read-only).
    ///
    /// With no paths, compares the two newest config backups. Use this after
    /// switching the vendor app into a per-key effect to see exactly which
    /// bytes the firmware changed.
    ConfigDiff {
        /// Older config file. Defaults to the second-newest backup.
        before: Option<PathBuf>,

        /// Newer config file. Defaults to the newest backup.
        after: Option<PathBuf>,
    },

    /// Switch the board to host-streamed per-key mode (non-volatile).
    ///
    /// Reads the keyboard's own config twice, saves a backup, patches only the
    /// three documented mode bytes, writes the block, then verifies the
    /// readback. If verification fails the original block is restored.
    PerKeyMode {
        /// Directory for the automatic backup. Defaults to %APPDATA%\rgbd\backups.
        #[arg(long)]
        backup_dir: Option<PathBuf>,

        /// Show the planned byte changes without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },

    /// Restore a 128-byte config backup (non-volatile).
    ///
    /// Only accepts files named like this tool's own F99 backups unless
    /// --force-file is passed. Always saves the current config first.
    RestoreConfig {
        /// Backup to restore.
        path: PathBuf,

        /// Directory for the automatic backup of the current config.
        #[arg(long)]
        backup_dir: Option<PathBuf>,

        /// Accept a file whose name does not match the rgbd backup pattern.
        #[arg(long)]
        force_file: bool,

        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },

    /// Stream a solid colour at the safe 20 FPS pacing.
    Solid {
        /// Colour as #RRGGBB, RRGGBB, #RGB, or RGB.
        #[arg(long, default_value = "#ff0000")]
        color: String,

        /// How long to stream. Use 0 to stream until interrupted.
        #[arg(long, default_value_t = 5)]
        seconds: u64,

        /// Requested frame rate. Clamped to the hardware ceiling of 20 FPS.
        #[arg(long, default_value_t = 20)]
        fps: u32,

        /// Resend every frame at the requested rate instead of change-only.
        ///
        /// Diagnostic only: continuous writes are noisy and do not improve a
        /// steady colour. The default matches OpenRGB: send on change, refresh
        /// at the 1 s keepalive.
        #[arg(long)]
        continuous: bool,
    },

    /// Cycle test colours, then sweep one LED at a time to verify mapping.
    Test {
        /// How long to hold each solid test colour.
        #[arg(long, default_value_t = 2)]
        seconds_per_color: u64,

        /// How long to hold each single-LED sweep step.
        #[arg(long, default_value_t = 150)]
        sweep_hold_ms: u64,

        /// Skip the single-LED sweep.
        #[arg(long)]
        no_sweep: bool,
    },
}

#[derive(Subcommand, Debug)]
enum RazerSetting {
    /// Set the mouse polling rate in Hz.
    Poll {
        hz: u16,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },

    /// Set DPI; if y is omitted, use x for both axes.
    Dpi {
        x: u16,
        y: Option<u16>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },

    /// Set the logo brightness from 0 to 255.
    Brightness {
        value: u8,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },

    /// Set a static logo colour (unverified and not reversible).
    Static {
        color: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        accept_unverified: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Probe => probe(),
        Command::RazerProbe { raw } => razer_probe(raw),
        Command::RazerSet { setting } => razer_set(setting),
        Command::Config { backup_dir } => read_config(backup_dir),
        Command::LedMap { backup_dir } => read_led_map(backup_dir),
        Command::ConfigDiff { before, after } => config_diff(before, after),
        Command::PerKeyMode {
            backup_dir,
            dry_run,
            yes,
        } => per_key_mode(backup_dir, dry_run, yes),
        Command::RestoreConfig {
            path,
            backup_dir,
            force_file,
            yes,
        } => restore_config(&path, backup_dir, force_file, yes),
        Command::Solid {
            color,
            seconds,
            fps,
            continuous,
        } => solid(&color, seconds, fps, continuous),
        Command::Test {
            seconds_per_color,
            sweep_hold_ms,
            no_sweep,
        } => test(seconds_per_color, sweep_hold_ms, no_sweep),
    }
}

fn probe() -> Result<()> {
    println!("AULA F99 / SinoWealth 258A:010C probe (read-only)");
    println!();

    let manager = HidManager::new()?;
    let collections = manager.aula_collections();

    if collections.is_empty() {
        bail!("no 258A:010C HID collections found; is the keyboard plugged in in wired mode?");
    }

    println!("HID collections on 258A:010C ({}):", collections.len());
    for collection in &collections {
        println!(
            "  usage_page=0x{:04X} ({}) usage=0x{:04X} iface={} product={:?}",
            collection.usage_page,
            usage_page_name(collection.usage_page),
            collection.usage,
            collection.interface,
            collection.product
        );
        println!("    {}", collection.path_display);
    }
    println!();

    let vendor = manager.aula_vendor_collections();
    if vendor.is_empty() {
        bail!("no vendor-page (0xFF00) collections; cannot reach the RGB protocol");
    }

    println!("Probing {} vendor collection(s):", vendor.len());
    let mut successes = 0usize;

    for candidate in &vendor {
        println!();
        println!("  Trying iface={} ...", candidate.interface);
        match Keyboard::open_candidate(&manager, candidate) {
            Ok(keyboard) => {
                successes += 1;
                println!("    OK: config signature and two identical reads confirmed");

                match keyboard.model_id {
                    Some(AULA_F99_MODEL_ID) => {
                        println!(
                            "    Model ID: 0x{:02X} ({})",
                            AULA_F99_MODEL_ID,
                            model_name(AULA_F99_MODEL_ID)
                        );
                    }
                    Some(id) => {
                        println!("    Model ID: 0x{id:02X} ({})", model_name(id));
                    }
                    None => println!("    Model ID: unavailable (config still valid)"),
                }

                println!(
                    "    Config: {} bytes, ends {:02X} {:02X}",
                    CONFIG_LEN, CONFIG_MAGIC[0], CONFIG_MAGIC[1]
                );
                match keyboard.query_model_raw() {
                    Ok(response) => {
                        let preview: Vec<String> = response
                            .iter()
                            .take(32)
                            .map(|byte| format!("{byte:02X}"))
                            .collect();
                        println!(
                            "    Model response ({} bytes): {}",
                            response.len(),
                            preview.join(" ")
                        );
                    }
                    Err(err) => println!("    Model response failed: {err:#}"),
                }
                match keyboard.mode_bytes() {
                    Some(mode) => println!(
                        "    Mode bytes [9,10,59] = {:02X} {:02X} {:02X}{}",
                        mode[0],
                        mode[1],
                        mode[2],
                        if keyboard.per_key_mode_hint() == Some(true) {
                            " (documented per-key values)"
                        } else {
                            ""
                        }
                    ),
                    None => println!("    Mode bytes: unavailable (config read did not stabilise)"),
                }
            }
            Err(err) => {
                println!("    no: {err:#}");
            }
        }
    }

    if successes == 0 {
        bail!("no vendor collection answered the RGB protocol");
    }

    println!();
    println!("Probe complete. Nothing was written to the keyboard.");
    Ok(())
}

fn razer_probe(raw: bool) -> Result<()> {
    println!("Razer Viper Mini / 1532:008A probe (read-only)");
    println!();

    let manager = HidManager::new()?;
    let collections = manager.razer_collections();

    if collections.is_empty() {
        bail!("no 1532:008A HID collections found; is the Viper Mini plugged in?");
    }

    println!("HID collections on 1532:008A ({}):", collections.len());
    for collection in &collections {
        println!(
            "  usage_page=0x{:04X} ({}) usage=0x{:04X} iface={} product={:?}",
            collection.usage_page,
            usage_page_name(collection.usage_page),
            collection.usage,
            collection.interface,
            collection.product
        );
        println!("    {}", collection.path_display);
    }
    println!();

    let mut successes = 0usize;

    for candidate in &collections {
        println!(
            "Trying iface={} usage_page=0x{:04X} usage=0x{:04X} ...",
            candidate.interface, candidate.usage_page, candidate.usage
        );

        match RazerDevice::open_candidate(&manager, candidate) {
            Ok(device) => {
                successes += 1;
                println!(
                    "  OK: transaction ID 0x{:02X}, {}",
                    device.transaction_id, device.path
                );

                let firmware = razer_raw_request(
                    &device,
                    raw,
                    "Firmware",
                    razer_class::STANDARD,
                    razer_cmd::GET_FIRMWARE,
                    2,
                    &[],
                )
                .and_then(|response| {
                    razer_require_success(&response, "firmware")?;
                    Ok((response.args[0], response.args[1]))
                });
                match firmware {
                    Ok((major, minor)) => println!("    Firmware: v{major}.{minor}"),
                    Err(err) => println!("    Firmware: FAILED: {err:#}"),
                }
                let serial = razer_raw_request(
                    &device,
                    raw,
                    "Serial",
                    razer_class::STANDARD,
                    razer_cmd::GET_SERIAL,
                    22,
                    &[],
                )
                .and_then(|response| {
                    razer_require_success(&response, "serial number")?;
                    let bytes: Vec<u8> = response.args[..22]
                        .iter()
                        .copied()
                        .take_while(|byte| *byte != 0)
                        .collect();
                    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
                });
                match serial {
                    Ok(serial) => println!("    Serial: {serial}"),
                    Err(err) => println!("    Serial: FAILED: {err:#}"),
                }
                let mode = razer_raw_request(
                    &device,
                    raw,
                    "Mode",
                    razer_class::STANDARD,
                    razer_cmd::GET_MODE,
                    2,
                    &[],
                )
                .and_then(|response| {
                    razer_require_success(&response, "device mode")?;
                    Ok(response.args[0])
                });
                match mode {
                    Ok(mode) => println!("    Mode: 0x{mode:02X} ({})", razer::mode_name(mode)),
                    Err(err) => println!("    Mode: FAILED: {err:#}"),
                }
                let poll_rate = razer_raw_request(
                    &device,
                    raw,
                    "Polling",
                    razer_class::STANDARD,
                    razer_cmd::GET_POLL_RATE,
                    1,
                    &[],
                )
                .and_then(|response| {
                    razer_require_success(&response, "polling rate")?;
                    let value = response.args[0];
                    Ok((value, razer::poll_rate_from_byte(value)))
                });
                match poll_rate {
                    Ok((raw, Some(hz))) => {
                        println!("    Polling rate: {hz} Hz (byte 0x{raw:02X})")
                    }
                    Ok((raw, None)) => {
                        println!("    Polling rate: unknown byte 0x{raw:02X}")
                    }
                    Err(err) => println!("    Polling rate: FAILED: {err:#}"),
                }
                let dpi = razer_raw_request(
                    &device,
                    raw,
                    "DPI",
                    razer_class::DPI,
                    razer_cmd::GET_DPI,
                    7,
                    &[razer::NOSTORE],
                )
                .and_then(|response| {
                    razer_require_success(&response, "DPI")?;
                    Ok((
                        u16::from_be_bytes([response.args[1], response.args[2]]),
                        u16::from_be_bytes([response.args[3], response.args[4]]),
                    ))
                });
                match dpi {
                    Ok((x, y)) => println!("    Current DPI: {x} x {y}"),
                    Err(err) => println!("    Current DPI: FAILED: {err:#}"),
                }
                let dpi_stages = razer_raw_request(
                    &device,
                    raw,
                    "DPI stages",
                    razer_class::DPI,
                    razer_cmd::GET_DPI_STAGES,
                    38,
                    &[razer::VARSTORE],
                )
                .and_then(|response| {
                    razer_require_success(&response, "DPI stages")?;
                    razer::parse_dpi_stages(&response.args)
                });
                match dpi_stages {
                    Ok(stages) => {
                        println!(
                            "    DPI stages: active {}, {} configured",
                            stages.active_stage, stages.count
                        );
                        for (index, (x, y)) in stages.stages.iter().enumerate() {
                            println!("      stage {}: {x} x {y}", index + 1);
                        }
                    }
                    Err(err) => println!("    DPI stages: FAILED: {err:#}"),
                }
                let brightness = razer_raw_request(
                    &device,
                    raw,
                    "Brightness",
                    razer_class::MATRIX,
                    razer_cmd::GET_BRIGHTNESS,
                    3,
                    &[razer::VARSTORE, razer::LOGO_LED],
                )
                .and_then(|response| {
                    razer_require_success(&response, "logo brightness")?;
                    Ok(response.args[2])
                });
                match brightness {
                    Ok(brightness) => {
                        let percent = (f32::from(brightness) / 255.0 * 100.0).round();
                        println!("    Logo brightness: {brightness}/255 (~{percent:.0}%)")
                    }
                    Err(err) => println!("    Logo brightness: FAILED: {err:#}"),
                }
            }
            Err(err) => println!("  no: {err:#}"),
        }

        println!();
    }

    if successes == 0 {
        bail!("no Viper Mini collection answered the Razer protocol");
    }

    println!("Probe complete. Nothing was written to the mouse.");
    Ok(())
}

fn razer_raw_request(
    device: &RazerDevice,
    raw: bool,
    label: &str,
    class: u8,
    command: u8,
    data_size: u8,
    args: &[u8],
) -> Result<razer::Response> {
    let response = device.request(class, command, data_size, args)?;

    if raw {
        let request_args = args
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        let body = response
            .raw()
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        let response_args = response
            .args
            .iter()
            .take(usize::from(response.data_size).min(response.args.len()))
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        println!("    {label} raw diagnostic:");
        println!(
            "      Request: class=0x{class:02X} cmd=0x{command:02X} data_size={data_size} args=[{request_args}]"
        );
        println!(
            "      Response status: 0x{:02X} ({})",
            response.status,
            response.status_name()
        );
        println!(
            "      Response transaction ID: 0x{:02X}",
            response.transaction_id
        );
        println!("      Response data_size: {}", response.data_size);
        println!("      Response class: 0x{:02X}", response.class);
        println!("      Response command: 0x{:02X}", response.command);
        println!(
            "      CRC: {}",
            if response.crc_ok { "ok" } else { "MISMATCH" }
        );
        println!(
            "      Response body ({} bytes): {body}",
            response.raw().len()
        );
        println!(
            "      Response args ({} bytes): [{response_args}]",
            response.args.len().min(usize::from(response.data_size))
        );
    }

    Ok(response)
}

fn razer_require_success(response: &razer::Response, what: &str) -> Result<()> {
    if !response.is_success() {
        bail!(
            "{what} request failed: {} (status 0x{:02X})",
            response.status_name(),
            response.status
        );
    }
    Ok(())
}

fn razer_set(setting: RazerSetting) -> Result<()> {
    let manager = HidManager::new()?;
    let device = RazerDevice::open(&manager)?;

    println!("Razer Viper Mini guarded setting");
    println!("Device path: {}", device.path);
    println!(
        "Control collection: interface={} usage_page=0x{:04X} usage=0x{:04X}",
        device.interface, device.usage_page, device.usage
    );
    println!("Discovered transaction ID: 0x{:02X}", device.transaction_id);
    println!();

    match setting {
        RazerSetting::Poll { hz, dry_run, yes } => razer_set_poll(&device, hz, dry_run, yes),
        RazerSetting::Dpi { x, y, dry_run, yes } => {
            razer_set_dpi(&device, x, y.unwrap_or(x), dry_run, yes)
        }
        RazerSetting::Brightness {
            value,
            dry_run,
            yes,
        } => razer_set_brightness(&device, value, dry_run, yes),
        RazerSetting::Static {
            color,
            dry_run,
            yes,
            accept_unverified,
        } => razer_set_static(&device, &color, dry_run, yes, accept_unverified),
    }
}

fn razer_set_poll(device: &RazerDevice, hz: u16, dry_run: bool, yes: bool) -> Result<()> {
    let target_byte = razer::poll_rate_to_byte(hz)?;
    let (current_byte, current) = device.poll_rate()?;
    let current = current.ok_or_else(|| {
        anyhow!(
            "refusing to write: current polling rate byte 0x{current_byte:02X} is unsupported, so rollback is impossible"
        )
    })?;

    println!("Current polling rate: {current} Hz; target: {hz} Hz");
    if dry_run {
        print_razer_report(
            device.transaction_id,
            razer_class::STANDARD,
            razer_cmd::SET_POLL_RATE,
            1,
            &[target_byte],
        );
        print_poll_write_plan(current, hz);
        println!("Dry run only: no set report was sent.");
        return Ok(());
    }

    let action = if current == hz {
        format!("set polling to {hz} Hz")
    } else {
        format!("set polling to {hz} Hz (identity write {current} Hz first)")
    };
    confirm_razer_write(&action, yes)?;

    if current != hz {
        let outcome = device.set_poll_rate_guarded(current)?;
        print_razer_write_outcome("Identity preflight", &outcome);
    }
    let outcome = device.set_poll_rate_guarded(hz)?;
    print_razer_write_outcome("Target write", &outcome);
    Ok(())
}

fn print_poll_write_plan(current: u16, target: u16) {
    if current == target {
        println!(
            "Real-run plan: write the target once and verify it; the guarded setter attempts rollback to {current} Hz on mismatch."
        );
    } else {
        println!(
            "Real-run plan: write the current value ({current} Hz) first and verify it, then write the target ({target} Hz) and verify it; the guarded setter attempts rollback to {current} Hz on mismatch."
        );
    }
}

fn razer_set_dpi(device: &RazerDevice, x: u16, y: u16, dry_run: bool, yes: bool) -> Result<()> {
    let target_args = razer::dpi_write_args(x, y)?;
    let current = device.dpi()?;
    razer::dpi_write_args(current.0, current.1)
        .context("current DPI cannot be safely restored; refusing to write")?;

    println!("Current DPI: {}x{}; target: {x}x{y}", current.0, current.1);
    println!("DPI writes request onboard storage; unplug/replug persistence is untested.");
    if dry_run {
        print_razer_report(
            device.transaction_id,
            razer_class::DPI,
            razer_cmd::SET_DPI,
            7,
            &target_args,
        );
        print_dpi_write_plan(current, (x, y));
        println!("Dry run only: no set report was sent.");
        return Ok(());
    }

    let action = if current == (x, y) {
        format!("set DPI to {x}x{y}")
    } else {
        format!(
            "set DPI to {x}x{y} (identity write {}x{} first)",
            current.0, current.1
        )
    };
    confirm_razer_write(&action, yes)?;

    if current != (x, y) {
        let outcome = device.set_dpi_guarded(current.0, current.1)?;
        print_razer_write_outcome("Identity preflight", &outcome);
    }
    let outcome = device.set_dpi_guarded(x, y)?;
    print_razer_write_outcome("Target write", &outcome);
    Ok(())
}

fn print_dpi_write_plan(current: (u16, u16), target: (u16, u16)) {
    if current == target {
        println!(
            "Real-run plan: write the target once and verify it; the guarded setter attempts rollback to {}x{} on mismatch.",
            current.0, current.1
        );
    } else {
        println!(
            "Real-run plan: write the current value ({}x{}) first and verify it, then write the target ({}x{}) and verify it; the guarded setter attempts rollback to {}x{} on mismatch.",
            current.0, current.1, target.0, target.1, current.0, current.1
        );
    }
}

fn razer_set_brightness(device: &RazerDevice, target: u8, dry_run: bool, yes: bool) -> Result<()> {
    let current = device.logo_brightness()?;
    println!("Current logo brightness: {current}/255; target: {target}/255");
    if dry_run {
        print_razer_report(
            razer::LIGHTING_TRANSACTION_ID,
            razer_class::MATRIX,
            razer_cmd::SET_BRIGHTNESS,
            3,
            &razer::brightness_write_args(target),
        );
        print_brightness_write_plan(current, target);
        println!("Dry run only: no set report was sent.");
        return Ok(());
    }

    let action = if current == target {
        format!("set logo brightness to {target}/255")
    } else {
        format!("set logo brightness to {target}/255 (identity write {current}/255 first)")
    };
    confirm_razer_write(&action, yes)?;

    if current != target {
        let outcome = device.set_logo_brightness_guarded(current)?;
        print_razer_write_outcome("Identity preflight", &outcome);
    }
    let outcome = device.set_logo_brightness_guarded(target)?;
    print_razer_write_outcome("Target write", &outcome);
    Ok(())
}

fn print_brightness_write_plan(current: u8, target: u8) {
    if current == target {
        println!(
            "Real-run plan: write the target once and verify it; the guarded setter attempts rollback to {current}/255 on mismatch."
        );
    } else {
        println!(
            "Real-run plan: write the current value ({current}/255) first and verify it, then write the target ({target}/255) and verify it; the guarded setter attempts rollback to {current}/255 on mismatch."
        );
    }
}

fn razer_set_static(
    device: &RazerDevice,
    color: &str,
    dry_run: bool,
    yes: bool,
    accept_unverified: bool,
) -> Result<()> {
    let color = Rgb::from_hex(color)?;
    let (red, green, blue) = (color.r, color.g, color.b);
    let color_hex = color.to_hex();

    println!("Target static logo colour: {color_hex}");
    println!(
        "WARNING: static colour has no documented readback and cannot be rolled back to the previous colour; brightness is checked before and after, but the colour itself is not verifiable."
    );
    if dry_run {
        print_razer_report(
            razer::LIGHTING_TRANSACTION_ID,
            razer_class::MATRIX,
            razer_cmd::SET_EFFECT,
            9,
            &razer::static_write_args(red, green, blue),
        );
        println!(
            "A real run requires both --accept-unverified and --yes; brightness will be checked before and after, but colour cannot be verified or rolled back."
        );
        println!("Dry run only: no set report was sent.");
        return Ok(());
    }

    if !accept_unverified {
        bail!(
            "refusing static colour write: pass --accept-unverified to acknowledge that colour cannot be verified or rolled back"
        );
    }
    if !yes {
        bail!(
            "refusing static colour write without --yes; an unverified static colour requires both --accept-unverified and --yes"
        );
    }

    let action = format!("write static colour {color_hex} (colour cannot be rolled back)");
    confirm_razer_write(&action, yes)?;
    device.set_logo_static(red, green, blue)?;
    println!("Static colour command succeeded; brightness was unchanged on readback.");
    println!("The colour itself is not readback-verifiable and cannot be rolled back.");
    Ok(())
}

fn print_razer_report(transaction_id: u8, class: u8, command: u8, data_size: u8, args: &[u8]) {
    let report = razer::build_report(transaction_id, class, command, data_size, args);
    let args = args
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "Proposed report: tx=0x{transaction_id:02X} class=0x{class:02X} cmd=0x{command:02X} size={data_size} args=[{args}] crc=0x{:02X}",
        report[razer::CRC_OFFSET]
    );
}

fn print_razer_write_outcome<T: std::fmt::Debug>(label: &str, outcome: &razer::WriteOutcome<T>) {
    println!(
        "{label}: previous={:?}, requested={:?}, readback={:?}",
        outcome.previous, outcome.requested, outcome.readback
    );
}

fn confirm_razer_write(action: &str, yes: bool) -> Result<()> {
    use std::io::{self, IsTerminal, Write};

    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!("refusing to {action} in a non-interactive shell without --yes");
    }

    print!("Type 'yes' to {action}: ");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("failed to read confirmation")?;
    let answer = answer.trim_end_matches(['\r', '\n']);

    if answer.eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        bail!("cancelled; no Razer write was sent")
    }
}

fn read_config(backup_dir: Option<PathBuf>) -> Result<()> {
    println!("Reading AULA F99 config (read-only)");
    let manager = HidManager::new()?;
    let keyboard = Keyboard::open(&manager)?;

    let config = keyboard.read_config_stable()?;
    if !is_valid_config(&config) {
        bail!("config failed validation after a stable read");
    }

    println!("Path: {}", keyboard.path);
    match keyboard.model_id {
        Some(id) => println!("Model: 0x{id:02X} ({})", model_name(id)),
        None => println!("Model: unavailable"),
    }
    let mode = mode_of(&config);
    println!(
        "Mode bytes [9,10,59] = {:02X} {:02X} {:02X}{}",
        mode[0],
        mode[1],
        mode[2],
        if is_per_key_mode(&config) {
            " (host-streamed per-key mode)"
        } else {
            ""
        }
    );
    println!();

    hexdump(&config);

    let dir = backup_dir.unwrap_or_else(default_backup_dir);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create backup directory {}", dir.display()))?;
    let path = dir.join(format!("aula-f99-{}-config.bin", timestamp()));
    fs::write(&path, config)
        .with_context(|| format!("failed to write backup {}", path.display()))?;

    println!();
    println!("Backup written to {}", path.display());
    Ok(())
}

/// Byte-level diff between two config backups.
///
/// The point is to learn the F99's own mode encoding from a known-good state
/// instead of copying the F75 values. Only offsets that actually changed are
/// printed, so a vendor-app switch produces a short, reviewable list.
fn config_diff(before: Option<PathBuf>, after: Option<PathBuf>) -> Result<()> {
    let (before_path, after_path) = match (before, after) {
        (Some(before), Some(after)) => (before, after),
        (None, None) => newest_config_pair()?,
        _ => bail!("pass both config files, or neither to use the two newest backups"),
    };

    let before = fs::read(&before_path)
        .with_context(|| format!("failed to read {}", before_path.display()))?;
    let after = fs::read(&after_path)
        .with_context(|| format!("failed to read {}", after_path.display()))?;

    println!("Config diff (read-only)");
    println!(
        "  before: {} ({} bytes)",
        before_path.display(),
        before.len()
    );
    println!("  after:  {} ({} bytes)", after_path.display(), after.len());
    println!();

    if before.len() != after.len() {
        bail!(
            "configs are different lengths ({} vs {}); refusing to compare",
            before.len(),
            after.len()
        );
    }
    if !is_valid_config(&before) || !is_valid_config(&after) {
        bail!("one of the files is not a valid config block (missing 5A A5 signature)");
    }

    let before: [u8; CONFIG_LEN] = before.try_into().expect("length checked above");
    let after: [u8; CONFIG_LEN] = after.try_into().expect("length checked above");
    let changes = config_changes(&before, &after);

    if changes.is_empty() {
        println!("No differences. The mode was unchanged between these backups.");
        return Ok(());
    }

    println!("{} byte(s) changed:", changes.len());
    for (index, old, new) in &changes {
        let note = match *index {
            9 => "  <- mode byte [9]",
            10 => "  <- mode byte [10]",
            59 => "  <- mode byte [59]",
            _ => "",
        };
        println!("  [{index:3}] 0x{index:02X}  {old:02X} -> {new:02X}{note}");
    }

    let before_mode = mode_of(&before);
    let after_mode = mode_of(&after);
    println!();
    println!(
        "Mode bytes [9,10,59]: {:02X} {:02X} {:02X} -> {:02X} {:02X} {:02X}",
        before_mode[0], before_mode[1], before_mode[2], after_mode[0], after_mode[1], after_mode[2]
    );
    if is_per_key_mode(&after) {
        println!("The newer config is in host-streamed per-key mode.");
    }
    Ok(())
}

/// Switch the board to host-streamed per-key mode.
///
/// This is the only non-volatile write in the tool, so every guard is explicit:
/// the model must be an F99, the block must come from this keyboard, only the
/// three documented mode bytes may change, and the write is read back before it
/// is trusted. If anything goes wrong the original block is restored.
fn per_key_mode(backup_dir: Option<PathBuf>, dry_run: bool, yes: bool) -> Result<()> {
    println!("AULA F99 per-key mode switch (non-volatile config write)");
    println!("Close the AULA F99 vendor app and any running rgbd before continuing.");
    println!();

    let manager = HidManager::new()?;
    let mut keyboard = Keyboard::open(&manager)?;
    require_f99(&keyboard)?;

    println!("Path: {}", keyboard.path);
    println!(
        "Model: 0x{:02X} ({})",
        AULA_F99_MODEL_ID,
        model_name(AULA_F99_MODEL_ID)
    );

    // read_config_stable requires two identical, valid reads. A glitched read
    // accepted here would write zeros over the whole config block.
    let original = keyboard.read_config_stable()?;
    let current_mode = mode_of(&original);
    println!(
        "Current mode bytes [9,10,59] = {:02X} {:02X} {:02X}",
        current_mode[0], current_mode[1], current_mode[2]
    );

    if is_per_key_mode(&original) {
        println!();
        println!("Already in per-key mode; nothing to write.");
        return Ok(());
    }

    let patched = with_mode(&original, MODE_PER_KEY)?;
    let changes = config_changes(&original, &patched);

    println!();
    println!("Planned change (only the lighting-mode bytes):");
    for (index, old, new) in &changes {
        println!("  [{index:3}] 0x{index:02X}  {old:02X} -> {new:02X}");
    }
    println!(
        "  mode [9,10,59]: {:02X} {:02X} {:02X} -> {:02X} {:02X} {:02X}",
        current_mode[0],
        current_mode[1],
        current_mode[2],
        MODE_PER_KEY[0],
        MODE_PER_KEY[1],
        MODE_PER_KEY[2]
    );

    if dry_run {
        println!();
        println!("Dry run: nothing was written.");
        return Ok(());
    }

    confirm_write(yes)?;

    let backup_path = write_config_backup(&original, backup_dir.as_deref(), "pre-mode")?;
    println!();
    println!("Backup: {}", backup_path.display());
    println!("Writing {CONFIG_LEN}-byte config...");

    if let Err(write_err) = keyboard.write_config(&patched) {
        // The report was not accepted, so the board is almost certainly
        // unchanged. Do not follow a failed write with another blind write.
        println!("Config write failed: {write_err:#}");
        println!("The original block is saved at {}", backup_path.display());
        println!(
            "If the board looks wrong, restore it with:\n  rgbctl restore-config \"{}\" --yes",
            backup_path.display()
        );
        return Err(write_err);
    }

    println!("Write sent; waited {CONFIG_SETTLE_MS} ms for the firmware's asynchronous repaint.");

    match verify_with_reconnect(&mut keyboard, &patched) {
        Ok(()) => {
            println!();
            println!("Verified: the readback matches the block that was written.");
            println!(
                "Mode bytes [9,10,59] = {:02X} {:02X} {:02X} (per-key).",
                MODE_PER_KEY[0], MODE_PER_KEY[1], MODE_PER_KEY[2]
            );
            println!("Stream a test frame with `rgbctl test --no-sweep`.");
            Ok(())
        }
        Err(verify_err) => {
            println!();
            println!("Verification failed: {verify_err:#}");
            println!("Restoring the original config...");

            match write_and_verify_with_reconnect(&mut keyboard, &original) {
                Ok(()) => {
                    println!("Original config restored and verified.");
                    println!("The per-key switch did not take; nothing else was changed.");
                    Err(anyhow!(
                        "per-key mode verification failed; the original config was restored"
                    ))
                }
                Err(restore_err) => {
                    println!();
                    println!("RESTORE FAILED: {restore_err:#}");
                    println!("The original block is saved at {}", backup_path.display());
                    println!("Do not stream colours. Retry the restore with:");
                    println!(
                        "  rgbctl restore-config \"{}\" --force-file --yes",
                        backup_path.display()
                    );
                    Err(anyhow!(
                        "per-key verification and automatic restore both failed: {restore_err:#}"
                    ))
                }
            }
        }
    }
}

/// Restore a config backup saved by this tool.
fn restore_config(
    path: &Path,
    backup_dir: Option<PathBuf>,
    force_file: bool,
    yes: bool,
) -> Result<()> {
    println!("AULA F99 config restore (non-volatile config write)");
    println!("Close the AULA F99 vendor app and any running rgbd before continuing.");
    println!();

    if !force_file && !is_rgbd_f99_backup(path) {
        bail!(
            "{} does not look like an rgbd AULA F99 backup; \
             pass --force-file only if you are certain it came from this keyboard",
            path.display()
        );
    }

    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() != CONFIG_LEN {
        bail!(
            "{} is {} bytes; a config block must be exactly {CONFIG_LEN}",
            path.display(),
            bytes.len()
        );
    }
    let restored: [u8; CONFIG_LEN] = bytes
        .as_slice()
        .try_into()
        .expect("config length was checked above");
    if !is_valid_config(&restored) {
        bail!(
            "{} is not a valid config block (missing 5A A5 signature)",
            path.display()
        );
    }

    println!("Restore source: {}", path.display());
    let source_mode = mode_of(&restored);
    println!(
        "Source mode bytes [9,10,59] = {:02X} {:02X} {:02X}",
        source_mode[0], source_mode[1], source_mode[2]
    );

    let manager = HidManager::new()?;
    let mut keyboard = Keyboard::open(&manager)?;
    require_f99(&keyboard)?;
    println!("Path: {}", keyboard.path);

    let current = keyboard.read_config_stable()?;
    let changes = config_changes(&current, &restored);
    println!();

    if changes.is_empty() {
        println!("Current config already matches the backup; nothing to write.");
        return Ok(());
    }

    let current_mode = mode_of(&current);
    println!("Planned change ({} byte(s)):", changes.len());
    for (index, old, new) in changes.iter().take(32) {
        println!("  [{index:3}] 0x{index:02X}  {old:02X} -> {new:02X}");
    }
    if changes.len() > 32 {
        println!("  ... and {} more", changes.len() - 32);
    }
    println!(
        "  mode [9,10,59]: {:02X} {:02X} {:02X} -> {:02X} {:02X} {:02X}",
        current_mode[0],
        current_mode[1],
        current_mode[2],
        source_mode[0],
        source_mode[1],
        source_mode[2]
    );

    confirm_write(yes)?;

    let backup_path = write_config_backup(&current, backup_dir.as_deref(), "pre-restore")?;
    println!("Current config backed up to {}", backup_path.display());

    match write_and_verify_with_reconnect(&mut keyboard, &restored) {
        Ok(()) => {
            println!();
            println!("Restored and verified: the readback matches the backup exactly.");
            Ok(())
        }
        Err(err) => {
            println!();
            println!("Restore write/verification failed: {err:#}");
            println!(
                "Your pre-restore config is saved at {}",
                backup_path.display()
            );
            Err(err)
        }
    }
}

fn require_f99(keyboard: &Keyboard) -> Result<()> {
    match keyboard.model_id {
        Some(AULA_F99_MODEL_ID) => Ok(()),
        Some(other) => {
            bail!("refusing a config write: model ID 0x{other:02X} is not an AULA F99 (0xA4)")
        }
        None => bail!("refusing a config write: the keyboard did not report a model ID"),
    }
}

/// Write a block and verify it, reopening the HID collection once if the config
/// write reset the link.
fn write_and_verify_with_reconnect(
    keyboard: &mut Keyboard,
    config: &[u8; CONFIG_LEN],
) -> Result<()> {
    keyboard.write_config(config)?;
    verify_with_reconnect(keyboard, config)
}

fn verify_with_reconnect(keyboard: &mut Keyboard, expected: &[u8; CONFIG_LEN]) -> Result<()> {
    match keyboard.verify_config(expected) {
        Ok(_) => Ok(()),
        Err(first) => {
            // A config write can briefly reset the HID link. Reopen once and
            // read again before declaring the write failed.
            thread::sleep(Duration::from_millis(500));
            let fresh = reopen_f99().map_err(|reopen_err| {
                anyhow!("{first:#}; reopening the keyboard failed: {reopen_err:#}")
            })?;

            match fresh.verify_config(expected) {
                Ok(_) => {
                    *keyboard = fresh;
                    Ok(())
                }
                Err(second) => {
                    *keyboard = fresh;
                    Err(second.context(format!("first readback failed: {first:#}")))
                }
            }
        }
    }
}

fn reopen_f99() -> Result<Keyboard> {
    let manager = HidManager::new()?;
    let keyboard = Keyboard::open(&manager)?;
    require_f99(&keyboard)?;
    Ok(keyboard)
}

fn confirm_write(yes: bool) -> Result<()> {
    use std::io::{self, IsTerminal, Write};

    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!("refusing a non-volatile config write without --yes in a non-interactive shell");
    }

    print!("Type 'yes' to write the keyboard config: ");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("failed to read confirmation")?;

    if answer.trim().eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        bail!("cancelled; nothing was written")
    }
}

fn write_config_backup(
    config: &[u8; CONFIG_LEN],
    backup_dir: Option<&Path>,
    tag: &str,
) -> Result<PathBuf> {
    let dir = backup_dir
        .map(PathBuf::from)
        .unwrap_or_else(default_backup_dir);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create backup directory {}", dir.display()))?;

    let path = dir.join(format!("aula-f99-{}-{tag}-config.bin", timestamp()));
    fs::write(&path, config)
        .with_context(|| format!("failed to write backup {}", path.display()))?;
    Ok(path)
}

fn is_rgbd_f99_backup(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with("aula-f99-") && name.ends_with("-config.bin"))
        .unwrap_or(false)
}

/// Find the two newest `*-config.bin` files in the default backup directory.
fn newest_config_pair() -> Result<(PathBuf, PathBuf)> {
    let dir = default_backup_dir();
    let mut configs: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("failed to list backup directory {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with("-config.bin"))
                .unwrap_or(false)
        })
        .collect();

    // Backup names embed a Unix timestamp, so lexicographic order is also
    // chronological order.
    configs.sort();

    if configs.len() < 2 {
        bail!(
            "need two config backups in {}; run `rgbctl config` before and after \
             switching the vendor app to a per-key effect",
            dir.display()
        );
    }

    let after = configs.pop().unwrap();
    let before = configs.pop().unwrap();
    Ok((before, after))
}

fn read_led_map(backup_dir: Option<PathBuf>) -> Result<()> {
    println!("Reading AULA F99 LED map (command 0x83, read-only)");
    let manager = HidManager::new()?;
    let keyboard = Keyboard::open(&manager)?;
    let map = keyboard.read_led_map()?;

    println!("Path: {}", keyboard.path);
    println!("Response payload: {} bytes", map.len());

    let entries = parse_led_map(&map);
    println!(
        "Entries: {} (4 bytes each: special, modifier mask, HID usage big-endian)",
        entries.len()
    );
    println!();

    for (index, entry) in entries.iter().enumerate() {
        let bytes = &map[index * 4..index * 4 + 4];
        println!(
            "  {:3}: {:<14} {:02X} {:02X} {:02X} {:02X}",
            index,
            entry.label(),
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3]
        );
    }

    let non_empty = entries
        .iter()
        .filter(|entry| !entry.is_empty() && !entry.is_footer())
        .count();
    println!();
    println!("Mapped key entries: {non_empty}");

    let dir = backup_dir.unwrap_or_else(default_backup_dir);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create backup directory {}", dir.display()))?;
    let path = dir.join(format!("aula-f99-{}-led-map.bin", timestamp()));
    fs::write(&path, &map)
        .with_context(|| format!("failed to write LED map {}", path.display()))?;

    println!("Raw map written to {}", path.display());
    Ok(())
}

fn solid(color: &str, seconds: u64, fps: u32, continuous: bool) -> Result<()> {
    let color = Rgb::from_hex(color)?;
    let manager = HidManager::new()?;
    let keyboard = Keyboard::open(&manager)?;

    println!("Connected: {}", keyboard.path);
    print_mode_warning(&keyboard);
    println!(
        "Streaming {} at a {} ms floor, {}...",
        color.to_hex(),
        pacing_gap_ms(fps),
        if continuous {
            "continuous diagnostic mode"
        } else {
            "change-only + 1 s keepalive"
        }
    );
    print_writer_warning();
    println!();

    let mut streamer = Streamer::new(&keyboard, fps).with_continuous(continuous);
    let frame = vec![color; STREAM_SLOTS];

    if seconds == 0 {
        println!("Streaming until interrupted. Press Ctrl+C to stop.");
        loop {
            streamer.run_for(&frame, Duration::from_secs(1))?;
        }
    } else {
        streamer.run_for(&frame, Duration::from_secs(seconds))?;
        println!("Done. 0x08 is volatile; the board may revert to its stored effect now.");
    }

    Ok(())
}

fn test(seconds_per_color: u64, sweep_hold_ms: u64, no_sweep: bool) -> Result<()> {
    let manager = HidManager::new()?;
    let keyboard = Keyboard::open(&manager)?;

    println!("Connected: {}", keyboard.path);
    print_mode_warning(&keyboard);
    print_writer_warning();
    println!();

    let mut streamer = Streamer::new(&keyboard, MAX_FPS);

    let colors = [
        ("red", Rgb::RED),
        ("green", Rgb::GREEN),
        ("blue", Rgb::BLUE),
        ("white", Rgb::WHITE),
    ];

    for (name, color) in colors {
        println!("Solid {name} ({}).", color.to_hex());
        let frame = vec![color; STREAM_SLOTS];
        streamer.run_for(&frame, Duration::from_secs(seconds_per_color))?;
    }

    if !no_sweep {
        println!();
        println!(
            "Single-LED sweep: slot 0..{}. Match each lit key against the LED map.",
            STREAM_SLOTS - 1
        );
        for slot in 0..STREAM_SLOTS {
            let mut frame = vec![Rgb::BLACK; STREAM_SLOTS];
            frame[slot] = Rgb::WHITE;
            println!("  slot {slot:3}");
            streamer.run_for(&frame, Duration::from_millis(sweep_hold_ms))?;
        }
    }

    println!();
    println!("Test complete. Streaming stopped; the board will revert to its stored effect.");
    Ok(())
}

/// Sends frames at the safe pacing interval.
///
/// The default is change-only plus a 1 s keepalive, matching OpenRGB's working
/// F99 driver: 0x08 is a volatile framebuffer, so a static frame needs an
/// occasional refresh but not 20 identical writes a second. `--continuous` is
/// kept as a diagnostic escape hatch.
struct Streamer<'a> {
    keyboard: &'a Keyboard,
    interval: Duration,
    next_write: Instant,
    last_sent: Option<Instant>,
    last_frame: Option<Vec<Rgb>>,
    continuous: bool,
}

impl<'a> Streamer<'a> {
    fn new(keyboard: &'a Keyboard, fps: u32) -> Self {
        let interval = Duration::from_millis(pacing_gap_ms(fps));
        Self {
            keyboard,
            interval,
            next_write: Instant::now(),
            last_sent: None,
            last_frame: None,
            continuous: false,
        }
    }

    fn with_continuous(mut self, continuous: bool) -> Self {
        self.continuous = continuous;
        self
    }

    fn send(&mut self, colors: &[Rgb], deadline: Option<Instant>) -> Result<()> {
        let changed = self.last_frame.as_deref() != Some(colors);
        let keepalive_due = self
            .last_sent
            .map(|sent| sent.elapsed() >= Duration::from_millis(KEEPALIVE_MS))
            .unwrap_or(true);

        if !self.continuous && !changed && !keepalive_due {
            return Ok(());
        }

        let now = Instant::now();
        if self.next_write > now {
            // Never start a write that would land at or after the caller's
            // deadline; a short run must not overrun its requested duration.
            if deadline.is_some_and(|deadline| self.next_write >= deadline) {
                return Ok(());
            }
            thread::sleep(self.next_write - now);
        }

        self.keyboard.send_stream(colors)?;
        let sent = Instant::now();
        self.last_sent = Some(sent);
        self.next_write = sent + self.interval;
        self.last_frame = Some(colors.to_vec());
        Ok(())
    }

    /// Run a constant frame for a duration. Frames are resent only on change
    /// or at the keepalive interval.
    fn run_for(&mut self, colors: &[Rgb], duration: Duration) -> Result<()> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.send(colors, Some(deadline))?;
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

fn pacing_gap_ms(requested_fps: u32) -> u64 {
    let fps = requested_fps.clamp(1, MAX_FPS);
    (1000 / u64::from(fps)).max(MIN_FRAME_GAP_MS)
}

fn print_writer_warning() {
    let conflicts = rgbd::conflicts::active_conflicts();
    if conflicts.is_empty() {
        println!("Keep SignalRGB, OpenRGB, and the AULA F99 software closed while testing.");
        return;
    }

    let names = conflicts
        .iter()
        .map(|conflict| conflict.label)
        .collect::<Vec<_>>()
        .join(", ");
    println!("WARNING: another lighting writer is running: {names}.");
    println!("Close it first; two writers alternate frames and make the board flicker.");
}

fn print_mode_warning(keyboard: &Keyboard) {
    match keyboard.mode_bytes() {
        Some(mode) => {
            println!(
                "Config mode bytes [9,10,59] = {:02X} {:02X} {:02X}",
                mode[0], mode[1], mode[2]
            );
            if keyboard.per_key_mode_hint() == Some(true) {
                println!("Board reports host-streamed per-key mode.");
            } else {
                println!(
                    "Board is not in per-key mode, so a firmware effect will repaint over \
                     streamed colours. Switch it with `rgbctl per-key-mode`."
                );
            }
        }
        None => {
            println!("Config mode bytes unavailable; streaming anyway.");
            println!(
                "If no colours appear, switch the board to per-key mode with \
                 `rgbctl per-key-mode`."
            );
        }
    }
}

fn default_backup_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rgbd")
        .join("backups")
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn hexdump(bytes: &[u8]) {
    for (offset, chunk) in bytes.chunks(16).enumerate() {
        print!("{:04X}  ", offset * 16);
        for byte in chunk {
            print!("{byte:02X} ");
        }
        for _ in chunk.len()..16 {
            print!("   ");
        }
        print!(" |");
        for byte in chunk {
            let ch = if (0x20..0x7F).contains(byte) {
                *byte as char
            } else {
                '.'
            };
            print!("{ch}");
        }
        println!("|");
    }
}
