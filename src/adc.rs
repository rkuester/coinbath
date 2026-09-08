//! ADS1015 driver over Linux I2C.
//!
//! Single-ended one-shot reads at the +/-4.096 V range, where the
//! 12-bit result is 2 mV per count. The wider range covers a
//! divider output that approaches the 3.3 V supply.

use std::fmt;

use anyhow::{Context, Result};
use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CDevice;
use serde::Deserialize;

/// A single-ended input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "u8")]
pub enum Channel {
    A0 = 0,
    A1 = 1,
    A2 = 2,
    A3 = 3,
}

impl TryFrom<u8> for Channel {
    type Error = String;

    fn try_from(n: u8) -> Result<Self, Self::Error> {
        match n {
            0 => Ok(Channel::A0),
            1 => Ok(Channel::A1),
            2 => Ok(Channel::A2),
            3 => Ok(Channel::A3),
            _ => Err(format!("channel {n} is not 0..=3")),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "A{}", *self as u8)
    }
}

/// An ADS1015 at the default address on one bus.
pub struct Ads1015 {
    dev: LinuxI2CDevice,
}

impl Ads1015 {
    /// Opens the chip on `bus`, such as "/dev/i2c-1".
    pub fn open(bus: &str) -> Result<Self> {
        let dev = LinuxI2CDevice::new(bus, ADDRESS)
            .with_context(|| format!("open {bus} at 0x{ADDRESS:02x}"))?;
        Ok(Self { dev })
    }

    /// Reads `channel` once and returns volts.
    pub fn read_voltage(&mut self, channel: Channel) -> Result<f64> {
        Ok(self.read_raw(channel)? as f64 * VOLTS_PER_COUNT)
    }

    /// Reads `channel` once and returns the signed 12-bit count.
    pub fn read_raw(&mut self, channel: Channel) -> Result<i16> {
        let config = config_word(channel);
        self.dev
            .write(&[REG_CONFIG, (config >> 8) as u8, config as u8])
            .context("start conversion")?;
        self.wait_ready()?;
        let mut buf = [0u8; 2];
        self.dev
            .write(&[REG_CONVERSION])
            .context("select conversion register")?;
        self.dev.read(&mut buf).context("read conversion")?;
        Ok(raw_from_bytes(buf))
    }

    fn wait_ready(&mut self) -> Result<()> {
        for _ in 0..MAX_POLLS {
            let mut buf = [0u8; 2];
            self.dev
                .write(&[REG_CONFIG])
                .context("select config register")?;
            self.dev.read(&mut buf).context("read config")?;
            if u16::from_be_bytes(buf) & CFG_OS != 0 {
                return Ok(());
            }
        }
        anyhow::bail!("conversion not ready after {MAX_POLLS} polls")
    }
}

const ADDRESS: u16 = 0x48;
const REG_CONVERSION: u8 = 0x00;
const REG_CONFIG: u8 = 0x01;
/// Start a conversion when written, conversion ready when read.
const CFG_OS: u16 = 1 << 15;
const CFG_PGA_4_096V: u16 = 0b001 << 9;
const CFG_MODE_SINGLE: u16 = 1 << 8;
const CFG_DR_1600SPS: u16 = 0b100 << 5;
const CFG_COMP_DISABLE: u16 = 0b11;
/// A conversion takes 0.625 ms at 1600 SPS; each poll is an I2C
/// round trip of about the same length.
const MAX_POLLS: u32 = 10;
const VOLTS_PER_COUNT: f64 = 4.096 / 2048.0;

/// The config word that starts a single-ended read of `channel`.
fn config_word(channel: Channel) -> u16 {
    // Single-ended inputs are MUX values 0b100..=0b111.
    let mux = (0b100 + channel as u16) << 12;
    CFG_OS | mux | CFG_PGA_4_096V | CFG_MODE_SINGLE | CFG_DR_1600SPS | CFG_COMP_DISABLE
}

/// The conversion register holds the 12-bit result left-aligned.
fn raw_from_bytes(buf: [u8; 2]) -> i16 {
    i16::from_be_bytes(buf) >> 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_word_matches_datasheet_example() {
        // OS=1, MUX=100 (AIN0), PGA=001, MODE=1, DR=100, COMP_QUE=11
        assert_eq!(config_word(Channel::A0), 0xC383);
        assert_eq!(config_word(Channel::A3), 0xF383);
    }

    #[test]
    fn raw_is_sign_extended() {
        assert_eq!(raw_from_bytes([0x7F, 0xF0]), 2047);
        assert_eq!(raw_from_bytes([0xFF, 0xF0]), -1);
        assert_eq!(raw_from_bytes([0x00, 0x00]), 0);
    }

    #[test]
    fn channel_parses_from_number() {
        assert_eq!(Channel::try_from(2).unwrap(), Channel::A2);
        assert!(Channel::try_from(4).is_err());
        let ch: Channel = serde_json::from_str("3").unwrap();
        assert_eq!(ch, Channel::A3);
    }
}
