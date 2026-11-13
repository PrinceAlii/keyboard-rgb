//! Per-user Windows sign-in startup entry. Reading status never changes it.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "rgbd";
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Enabled { command: String },
    Disabled,
    Error(String),
}

/// Always quote the executable, including paths without spaces. Windows paths
/// cannot contain double quotes, and this command has no additional arguments.
pub fn registered_command(executable: &Path) -> String {
    format!("\"{}\"", executable.display())
}

fn reg(args: &[&str]) -> Result<Output> {
    let system_root = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
    let executable = Path::new(&system_root).join("System32").join("reg.exe");
    Command::new(executable)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("Could not run reg.exe")
}

pub fn status() -> Status {
    match reg(&["query", RUN_KEY, "/v", VALUE_NAME]) {
        Ok(output) => parse_query(
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(error) => Status::Error(format!("{error:#}")),
    }
}

pub fn enable() -> Result<()> {
    let executable = std::env::current_exe().context("Could not locate the rgbd executable")?;
    let command = registered_command(&executable);
    let output = reg(&[
        "add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d", &command, "/f",
    ])?;
    if !output.status.success() {
        bail!("Could not enable startup: {}", output_error(&output));
    }
    Ok(())
}

pub fn disable() -> Result<()> {
    let output = reg(&["delete", RUN_KEY, "/v", VALUE_NAME, "/f"])?;
    if !output.status.success() && !is_not_found(&output_error(&output)) {
        bail!("Could not disable startup: {}", output_error(&output));
    }
    Ok(())
}

fn output_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = format!("{}\n{}", stderr.trim(), stdout.trim());
    let message = message.trim();
    if message.is_empty() {
        format!("reg.exe exited with {}", output.status)
    } else {
        message.to_string()
    }
}

fn is_not_found(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("unable to find the specified registry key or value")
        || message.contains("the system cannot find the file specified")
}

fn parse_query(success: bool, stdout: &str, stderr: &str) -> Status {
    if !success {
        let message = format!("{}\n{}", stderr.trim(), stdout.trim());
        return if is_not_found(&message) {
            Status::Disabled
        } else if message.trim().is_empty() {
            Status::Error("reg.exe query failed without an error message".to_string())
        } else {
            Status::Error(message.trim().to_string())
        };
    }

    for line in stdout.lines() {
        let line = line.trim();
        let Some(name_end) = line.find(char::is_whitespace) else {
            continue;
        };
        if !line[..name_end].eq_ignore_ascii_case(VALUE_NAME) {
            continue;
        }
        let rest = line[name_end..].trim_start();
        let type_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if rest[..type_end].eq_ignore_ascii_case("REG_SZ") {
            return Status::Enabled {
                command: rest[type_end..].trim().to_string(),
            };
        }
        return Status::Error("The rgbd startup value is not REG_SZ".to_string());
    }
    Status::Error("Could not parse the rgbd startup value from reg.exe output".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_executable_paths() {
        for path in [
            r"C:\rgbd.exe",
            r"C:\Program Files\rgbd\rgbd.exe",
            r"C:\Users\Zoë\rgbd.exe",
        ] {
            assert_eq!(registered_command(Path::new(path)), format!("\"{path}\""));
        }
    }

    #[test]
    fn parses_command_with_spaces_and_crlf() {
        let command = r#""C:\Program Files\rgbd\rgbd.exe""#;
        for separator in ["    ", "\t"] {
            let stdout =
                format!("\r\n{RUN_KEY}\r\n    rgbd{separator}REG_SZ{separator}{command}\r\n");
            assert_eq!(
                parse_query(true, &stdout, ""),
                Status::Enabled {
                    command: command.into()
                }
            );
        }
    }

    #[test]
    fn distinguishes_missing_value_from_errors() {
        let missing =
            "ERROR: The system was unable to find the specified registry key or value.\r\n";
        assert_eq!(parse_query(false, "", missing), Status::Disabled);
        assert_eq!(parse_query(false, missing, ""), Status::Disabled);
        assert!(matches!(
            parse_query(false, "", "ERROR: Access is denied.\r\n"),
            Status::Error(_)
        ));
        assert!(matches!(parse_query(false, "", ""), Status::Error(_)));
        assert!(matches!(
            parse_query(true, "unexpected output", ""),
            Status::Error(_)
        ));
        assert!(matches!(
            parse_query(true, "rgbd    REG_DWORD    1", ""),
            Status::Error(_)
        ));
    }
}
