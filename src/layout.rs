//! Physical layout of the AULA F99, transcribed from the vendor driver's
//! `KB.ini`.
//!
//! The coordinates are the ones the AULA F99 software uses to draw the board:
//! a 657 × 246 canvas with the origin at the top left. `led` is the onboard LED
//! index, which is also the slot index in the live `0x08` stream. That
//! 1:1 relationship is what makes per-key painting possible without a guess
//! table.

/// Width of the vendor's layout canvas.
pub const LAYOUT_WIDTH: f32 = 657.0;
/// Height of the vendor's layout canvas.
pub const LAYOUT_HEIGHT: f32 = 246.0;

/// One physical key: its label, its rectangle on the vendor canvas, and the
/// LED index it occupies in the streaming frame.
#[derive(Clone, Copy, Debug)]
pub struct KeyDef {
    pub label: &'static str,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub led: u8,
}

impl KeyDef {
    pub const fn width(&self) -> f32 {
        self.x2 - self.x1
    }

    pub const fn height(&self) -> f32 {
        self.y2 - self.y1
    }
}

const fn key(label: &'static str, x1: f32, y1: f32, x2: f32, y2: f32, led: u8) -> KeyDef {
    KeyDef {
        label,
        x1,
        y1,
        x2,
        y2,
        led,
    }
}

/// All 99 keys, in the vendor driver's drawing order.
pub const F99_KEYS: [KeyDef; 99] = [
    key("Esc", 18.0, 20.0, 40.0, 47.0, 0),
    key("F1", 60.0, 19.0, 82.0, 46.0, 6),
    key("F2", 92.0, 20.0, 114.0, 47.0, 12),
    key("F3", 123.0, 20.0, 145.0, 47.0, 18),
    key("F4", 156.0, 20.0, 178.0, 47.0, 24),
    key("F5", 195.0, 19.0, 217.0, 46.0, 30),
    key("F6", 229.0, 20.0, 251.0, 47.0, 36),
    key("F7", 261.0, 20.0, 283.0, 47.0, 42),
    key("F8", 294.0, 20.0, 316.0, 47.0, 48),
    key("F9", 334.0, 19.0, 356.0, 46.0, 54),
    key("F10", 367.0, 19.0, 389.0, 46.0, 60),
    key("F11", 400.0, 20.0, 422.0, 47.0, 66),
    key("F12", 432.0, 20.0, 454.0, 47.0, 72),
    key("`", 19.0, 63.0, 41.0, 90.0, 1),
    key("1", 51.0, 64.0, 73.0, 91.0, 7),
    key("2", 84.0, 64.0, 106.0, 91.0, 13),
    key("3", 116.0, 64.0, 138.0, 91.0, 19),
    key("4", 149.0, 64.0, 171.0, 91.0, 25),
    key("5", 181.0, 64.0, 203.0, 91.0, 31),
    key("6", 213.0, 64.0, 235.0, 91.0, 37),
    key("7", 246.0, 64.0, 268.0, 91.0, 43),
    key("8", 278.0, 64.0, 300.0, 91.0, 49),
    key("9", 311.0, 64.0, 333.0, 91.0, 55),
    key("0", 343.0, 64.0, 365.0, 91.0, 61),
    key("-", 375.0, 64.0, 397.0, 91.0, 67),
    key("=", 408.0, 64.0, 430.0, 91.0, 73),
    key("Bksp", 440.0, 64.0, 494.0, 91.0, 79),
    key("Del", 472.0, 19.0, 494.0, 46.0, 78),
    key("Home", 520.0, 20.0, 542.0, 47.0, 90),
    key("Num", 519.0, 63.0, 541.0, 90.0, 91),
    key("/", 551.0, 63.0, 573.0, 90.0, 97),
    key("*", 584.0, 63.0, 606.0, 90.0, 103),
    key("-", 616.0, 63.0, 638.0, 90.0, 109),
    key("Tab", 19.0, 97.0, 57.0, 124.0, 2),
    key("Q", 67.0, 97.0, 89.0, 124.0, 8),
    key("W", 100.0, 97.0, 122.0, 124.0, 14),
    key("E", 131.0, 97.0, 153.0, 124.0, 20),
    key("R", 165.0, 97.0, 187.0, 124.0, 26),
    key("T", 198.0, 97.0, 220.0, 124.0, 32),
    key("Y", 230.0, 97.0, 252.0, 124.0, 38),
    key("U", 262.0, 97.0, 284.0, 124.0, 44),
    key("I", 294.0, 97.0, 316.0, 124.0, 50),
    key("O", 326.0, 97.0, 348.0, 124.0, 56),
    key("P", 359.0, 97.0, 381.0, 124.0, 62),
    key("[", 391.0, 97.0, 413.0, 124.0, 68),
    key("]", 424.0, 97.0, 446.0, 124.0, 74),
    key("\\", 457.0, 97.0, 493.0, 124.0, 80),
    key("End", 552.0, 20.0, 574.0, 47.0, 96),
    key("PgUp", 585.0, 20.0, 607.0, 47.0, 102),
    key("7", 519.0, 97.0, 541.0, 124.0, 92),
    key("8", 552.0, 97.0, 574.0, 124.0, 98),
    key("9", 584.0, 97.0, 606.0, 124.0, 104),
    key("+", 616.0, 99.0, 638.0, 154.0, 110),
    key("Caps", 19.0, 128.0, 65.0, 155.0, 3),
    key("A", 75.0, 128.0, 97.0, 155.0, 9),
    key("S", 107.0, 128.0, 129.0, 155.0, 15),
    key("D", 140.0, 128.0, 162.0, 155.0, 21),
    key("F", 172.0, 128.0, 194.0, 155.0, 27),
    key("G", 205.0, 128.0, 227.0, 155.0, 33),
    key("H", 238.0, 128.0, 260.0, 155.0, 39),
    key("J", 270.0, 128.0, 292.0, 155.0, 45),
    key("K", 302.0, 128.0, 324.0, 155.0, 51),
    key("L", 334.0, 128.0, 356.0, 155.0, 57),
    key(";", 367.0, 128.0, 389.0, 155.0, 63),
    key("'", 399.0, 128.0, 421.0, 155.0, 69),
    key("Enter", 432.0, 128.0, 494.0, 155.0, 81),
    key("4", 519.0, 128.0, 541.0, 155.0, 93),
    key("5", 552.0, 128.0, 574.0, 155.0, 99),
    key("6", 584.0, 128.0, 606.0, 155.0, 105),
    key("Shift", 19.0, 160.0, 81.0, 187.0, 4),
    key("Z", 92.0, 160.0, 114.0, 187.0, 10),
    key("X", 124.0, 160.0, 146.0, 187.0, 16),
    key("C", 157.0, 160.0, 179.0, 187.0, 22),
    key("V", 189.0, 160.0, 211.0, 187.0, 28),
    key("B", 221.0, 160.0, 243.0, 187.0, 34),
    key("N", 253.0, 160.0, 275.0, 187.0, 40),
    key("M", 286.0, 160.0, 308.0, 187.0, 46),
    key(",", 319.0, 160.0, 341.0, 187.0, 52),
    key(".", 351.0, 160.0, 373.0, 187.0, 58),
    key("/", 383.0, 160.0, 405.0, 187.0, 64),
    key("Shift", 415.0, 161.0, 461.0, 188.0, 82),
    key("Up", 480.0, 166.0, 502.0, 193.0, 88),
    key("1", 520.0, 160.0, 542.0, 187.0, 94),
    key("2", 552.0, 160.0, 574.0, 187.0, 100),
    key("3", 585.0, 160.0, 607.0, 187.0, 106),
    key("Ctrl", 19.0, 193.0, 49.0, 220.0, 5),
    key("Win", 60.0, 194.0, 90.0, 221.0, 11),
    key("Alt", 100.0, 193.0, 130.0, 220.0, 17),
    key("Space", 138.0, 194.0, 334.0, 221.0, 35),
    key("Alt", 344.0, 192.0, 366.0, 219.0, 53),
    key("Fn", 376.0, 192.0, 398.0, 219.0, 59),
    key("Ctrl", 408.0, 192.0, 430.0, 219.0, 65),
    key("Left", 447.0, 198.0, 469.0, 225.0, 83),
    key("Down", 480.0, 198.0, 502.0, 225.0, 89),
    key("Right", 512.0, 199.0, 534.0, 226.0, 95),
    key("0", 552.0, 192.0, 574.0, 219.0, 101),
    key(".", 584.0, 192.0, 606.0, 219.0, 107),
    key("Enter", 616.0, 162.0, 638.0, 220.0, 112),
    key("PgDn", 616.0, 19.0, 638.0, 46.0, 108),
];

/// Look up a key by the LED slot it occupies.
pub fn key_by_led(led: u8) -> Option<&'static KeyDef> {
    F99_KEYS.iter().find(|key| key.led == led)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_has_99_unique_led_indices() {
        let mut slots: Vec<u8> = F99_KEYS.iter().map(|key| key.led).collect();
        assert_eq!(slots.len(), 99);
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 99);
        assert!(*slots.last().unwrap() <= 112);
    }

    #[test]
    fn every_key_fits_the_vendor_canvas() {
        for key in &F99_KEYS {
            assert!(key.x1 >= 0.0 && key.y1 >= 0.0);
            assert!(key.x2 <= LAYOUT_WIDTH && key.y2 <= LAYOUT_HEIGHT);
            assert!(key.width() > 0.0 && key.height() > 0.0);
        }
    }

    #[test]
    fn known_keys_map_to_the_documented_leds() {
        assert_eq!(key_by_led(0).unwrap().label, "Esc");
        assert_eq!(key_by_led(59).unwrap().label, "Fn");
        assert_eq!(key_by_led(35).unwrap().label, "Space");
        assert_eq!(key_by_led(112).unwrap().label, "Enter");
    }
}
