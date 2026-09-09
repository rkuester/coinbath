//! Configuration file handling.

use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::adc::Channel;
use crate::probes::{ProbeConfig, ProbesConfig};

/// Top-level configuration, read from a TOML file.
///
/// Unknown keys are an error, so a misspelled or misplaced key
/// stops the daemon instead of being ignored.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct File {
    setpoint_c: f64,
    #[serde(default = "default_api_listen")]
    api_listen: SocketAddr,
    #[serde(default = "default_mujina_url")]
    mujina_url: String,
    #[serde(default = "default_i2c_bus")]
    i2c_bus: String,
    #[serde(default = "default_vcc")]
    vcc: f64,
    #[serde(default)]
    vcc_channel: Option<Channel>,
    #[serde(default)]
    probe: Vec<ProbeConfig>,
}

/// The configuration the daemon runs with.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Target bath temperature in degrees Celsius.
    pub setpoint_c: f64,
    /// Address the HTTP JSON API listens on.
    pub api_listen: SocketAddr,
    /// Base URL of the miner's REST API.
    pub mujina_url: String,
    /// The ADC and its thermistors.
    pub probes: ProbesConfig,
}

fn default_i2c_bus() -> String {
    "/dev/i2c-1".to_string()
}

/// The nominal 3.3 V rail; measure it for accuracy.
fn default_vcc() -> f64 {
    3.3
}

/// Loopback only; Mujina is on 7785 next door.
fn default_api_listen() -> SocketAddr {
    "127.0.0.1:7786".parse().expect("literal address parses")
}

fn default_mujina_url() -> String {
    crate::mujina::DEFAULT_URL.to_string()
}

impl Config {
    /// Loads and validates the configuration file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read config file {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse config file {}", path.display()))
    }

    /// Parses and validates configuration from TOML text.
    ///
    /// The probe list may be empty here; the daemon requires it only
    /// when it reads the ADC.
    pub fn parse(text: &str) -> Result<Self> {
        let file: File = toml::from_str(text)?;
        let config = Self {
            setpoint_c: file.setpoint_c,
            api_listen: file.api_listen,
            mujina_url: file.mujina_url,
            probes: ProbesConfig {
                i2c_bus: file.i2c_bus,
                vcc: file.vcc,
                vcc_channel: file.vcc_channel,
                probes: file.probe,
            },
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.setpoint_c.is_finite(),
            "setpoint_c must be a finite number"
        );
        if !self.probes.probes.is_empty() {
            self.probes.validate()?;
        }
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
    fn api_listen_defaults_to_loopback() {
        let config = Config::parse("setpoint_c = 51.1\n").unwrap();
        assert!(config.api_listen.ip().is_loopback());

        let config = Config::parse("setpoint_c = 51.1\napi_listen = \"0.0.0.0:8080\"\n").unwrap();
        assert_eq!(config.api_listen.port(), 8080);
    }

    #[test]
    fn mujina_url_defaults_to_loopback_and_can_be_set() {
        let config = Config::parse("setpoint_c = 51.1\n").unwrap();
        assert_eq!(config.mujina_url, "http://127.0.0.1:7785");

        let config =
            Config::parse("setpoint_c = 51.1\nmujina_url = \"http://rig:7785\"\n").unwrap();
        assert_eq!(config.mujina_url, "http://rig:7785");
    }

    #[test]
    fn probes_are_optional_but_checked_when_present() {
        let config = Config::parse("setpoint_c = 51.1\n").unwrap();
        assert!(config.probes.probes.is_empty());

        let bad = "setpoint_c = 51.1\n[[probe]]\nname = \"x\"\nchannel = 9\n\
                   divider_resistor = 1.0\na = 0.0\nb = 0.0\nc = 0.0\n";
        assert!(Config::parse(bad).is_err());
    }

    #[test]
    fn rejects_unknown_top_level_keys() {
        let err = Config::parse("setpoint_c = 51.1\nsetpoint_f = 124.0\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("setpoint_f"), "{err}");
    }

    #[test]
    fn reference_config_lists_three_probes() {
        let config = Config::parse(include_str!("../coinbath.toml")).unwrap();
        assert_eq!(config.probes.names(), ["inlet", "outlet", "bath"]);
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
