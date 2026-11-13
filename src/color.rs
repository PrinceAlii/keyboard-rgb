use anyhow::{Result, bail};

/// A single RGB colour, 8 bits per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const RED: Self = Self::new(255, 0, 0);
    pub const GREEN: Self = Self::new(0, 255, 0);
    pub const BLUE: Self = Self::new(0, 0, 255);
    pub const WHITE: Self = Self::new(255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#RRGGBB`, `RRGGBB`, `#RGB`, or `RGB`.
    pub fn from_hex(value: &str) -> Result<Self> {
        let value = value.trim().trim_start_matches('#');
        let expanded = match value.len() {
            3 => {
                let mut out = String::with_capacity(6);
                for ch in value.chars() {
                    out.push(ch);
                    out.push(ch);
                }
                out
            }
            6 => value.to_string(),
            _ => bail!("expected a hex colour like #ff0088, got {value:?}"),
        };

        let bytes = hex::decode(&expanded)
            .map_err(|err| anyhow::anyhow!("invalid hex colour {value:?}: {err}"))?;
        Ok(Self::new(bytes[0], bytes[1], bytes[2]))
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}
