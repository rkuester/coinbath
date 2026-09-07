//! Configuration file handling.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Top-level configuration, read from a TOML file.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Config {
    /// Target bath temperature in degrees Celsius.
    pub setpoint_c: f64,
}

impl Config {
    /// Loads and validates the configuration file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read config file {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse config file {}", path.display()))
    }

    /// Parses and validates configuration from TOML text.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.setpoint_c.is_finite(),
            "setpoint_c must be a finite number"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setpoint() {
        let config = Config::parse("setpoint_c = 51.1\n").unwrap();
        assert_eq!(config.setpoint_c, 51.1);
    }

    #[test]
    fn rejects_missing_setpoint() {
        assert!(Config::parse("").is_err());
    }

    #[test]
    fn rejects_non_finite_setpoint() {
        assert!(Config::parse("setpoint_c = inf\n").is_err());
    }
}
