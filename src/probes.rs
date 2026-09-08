//! The thermistor reader, which feeds the state task from the ADC.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::adc::{Ads1015, Channel};
use crate::state::{Client, Command};
use crate::thermistor::Thermistor;

/// One thermistor on one ADC input, as configured.
///
/// Unknown keys are an error, so a misspelled or misplaced key
/// stops the daemon instead of silently dropping a probe.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    /// Probe name, such as "bath".
    pub name: String,
    /// The ADC input the divider feeds.
    pub channel: Channel,
    /// Correction added to the converted reading, in degrees
    /// Celsius, from calibration against a reference thermometer.
    #[serde(default)]
    pub offset_c: f64,
    /// The fixed resistor in the divider, in ohms.
    pub divider_resistor: f64,
    /// Steinhart-Hart coefficients.
    pub a: f64,
    pub b: f64,
    pub c: f64,
}

impl ProbeConfig {
    fn thermistor(&self) -> Thermistor {
        Thermistor {
            divider_resistor: self.divider_resistor,
            a: self.a,
            b: self.b,
            c: self.c,
        }
    }
}

/// Everything the reader needs from the configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbesConfig {
    /// The I2C bus the ADC is on.
    pub i2c_bus: String,
    /// The divider supply, in volts, used when `vcc_channel` is
    /// unset or reads out of range.
    pub vcc: f64,
    /// An ADC input wired to the divider supply, read every cycle
    /// so the conversion is ratiometric.
    pub vcc_channel: Option<Channel>,
    /// Probes in index order, which is the order the state reports.
    pub probes: Vec<ProbeConfig>,
}

impl ProbesConfig {
    /// Probe names in index order.
    pub fn names(&self) -> Vec<&str> {
        self.probes.iter().map(|p| p.name.as_str()).collect()
    }

    /// Rejects an empty, duplicated, or physically impossible setup.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.probes.is_empty(), "no probes configured");
        anyhow::ensure!(self.vcc > 0.0, "vcc must be positive");
        for (i, p) in self.probes.iter().enumerate() {
            anyhow::ensure!(!p.name.is_empty(), "probe {i} has no name");
            anyhow::ensure!(
                p.divider_resistor > 0.0,
                "probe {}: divider_resistor must be positive",
                p.name
            );
            anyhow::ensure!(
                self.vcc_channel != Some(p.channel),
                "probe {} shares channel {} with vcc_channel",
                p.name,
                p.channel
            );
            if let Some(dup) = self.probes[..i].iter().find(|q| q.channel == p.channel) {
                anyhow::bail!(
                    "probes {} and {} share channel {}",
                    dup.name,
                    p.name,
                    p.channel
                );
            }
        }
        Ok(())
    }
}

/// How often every probe is read.
const INTERVAL: Duration = Duration::from_millis(500);

/// Conversions averaged into one reading. With the miner running,
/// a single conversion wanders a few tenths of a degree from the
/// next; sixteen at 1600 SPS take about 30 ms per channel and
/// bring that under a tenth.
const CONVERSIONS_PER_READING: usize = 16;

/// The mean of `CONVERSIONS_PER_READING` conversions of `channel`.
fn read_mean(adc: &mut Ads1015, channel: Channel) -> Result<f64> {
    let mut sum = 0.0;
    for _ in 0..CONVERSIONS_PER_READING {
        sum += adc.read_voltage(channel)?;
    }
    Ok(sum / CONVERSIONS_PER_READING as f64)
}

/// Below this the thermistor is open; above `vcc` minus this it is
/// shorted. Either way there is no reading.
const FAULT_MARGIN_V: f64 = 0.05;

/// A measured supply outside this range is a wiring fault, and the
/// configured value stands in for it.
const SUPPLY_RANGE_V: std::ops::RangeInclusive<f64> = 2.5..=3.6;

/// Classifies a divider voltage as a reading or a fault.
fn classify(volts: f64, vcc: f64) -> Result<f64, &'static str> {
    if volts < FAULT_MARGIN_V {
        Err("open")
    } else if volts > vcc - FAULT_MARGIN_V {
        Err("shorted")
    } else {
        Ok(volts)
    }
}

/// Reads every probe on a timer, each reading the mean of a burst
/// of conversions, and reports it to the state task, until the
/// state task is gone.
///
/// An open or shorted probe is reported as no reading, and the
/// change is logged once. A failed bus read is logged and the
/// probe keeps its last value.
pub async fn run(client: Client, config: ProbesConfig) -> Result<()> {
    let mut adc =
        tokio::task::block_in_place(|| Ads1015::open(&config.i2c_bus)).context("open the ADC")?;
    let thermistors: Vec<Thermistor> = config.probes.iter().map(|p| p.thermistor()).collect();
    let mut faults: Vec<Option<&str>> = vec![None; config.probes.len()];
    let mut supply_fault = false;
    let mut ticker = tokio::time::interval(INTERVAL);
    loop {
        ticker.tick().await;
        let mut vcc = config.vcc;
        if let Some(channel) = config.vcc_channel {
            match tokio::task::block_in_place(|| read_mean(&mut adc, channel)) {
                Ok(volts) if SUPPLY_RANGE_V.contains(&volts) => {
                    if supply_fault {
                        tracing::info!(volts, "supply reading again");
                        supply_fault = false;
                    }
                    vcc = volts;
                    client.send(Command::Supply { volts }).await?;
                }
                Ok(volts) => {
                    if !supply_fault {
                        tracing::warn!(volts, fallback = config.vcc, "supply out of range");
                        supply_fault = true;
                    }
                }
                Err(e) => tracing::warn!(channel = %channel, "supply read failed: {e:#}"),
            }
        }
        for (index, probe) in config.probes.iter().enumerate() {
            let volts = match tokio::task::block_in_place(|| read_mean(&mut adc, probe.channel)) {
                Ok(volts) => volts,
                Err(e) => {
                    tracing::warn!(name = %probe.name, channel = %probe.channel, "read failed: {e:#}");
                    continue;
                }
            };
            let reading = classify(volts, vcc);
            let fault = reading.err();
            if fault != faults[index] {
                match fault {
                    Some(fault) => {
                        tracing::warn!(name = %probe.name, channel = %probe.channel, volts, "probe {fault}")
                    }
                    None => {
                        tracing::info!(name = %probe.name, channel = %probe.channel, volts, "probe reading again")
                    }
                }
                faults[index] = fault;
            }
            let celsius = reading
                .ok()
                .map(|volts| thermistors[index].celsius(volts, vcc) + probe.offset_c);
            tracing::trace!(name = %probe.name, volts, vcc, ?celsius, "probe read");
            client
                .send(Command::ProbeReading {
                    index,
                    volts: Some(volts),
                    celsius,
                })
                .await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ProbesConfig> {
        let config = crate::config::Config::parse(&format!("setpoint_c = 50.0\n{text}"))?;
        config.probes.validate()?;
        Ok(config.probes)
    }

    const ONE: &str = r#"
        [[probe]]
        name = "bath"
        channel = 0
        divider_resistor = 10000.0
        a = 1.0e-3
        b = 2.5e-4
        c = 0.0
        offset_c = 0.0
    "#;

    #[test]
    fn parses_probes_with_defaults() {
        let config = parse(ONE).unwrap();
        assert_eq!(config.i2c_bus, "/dev/i2c-1");
        assert_eq!(config.vcc, 3.3);
        assert_eq!(config.names(), ["bath"]);
        assert_eq!(config.probes[0].channel, Channel::A0);
        assert_eq!(config.probes[0].offset_c, 0.0);
    }

    #[test]
    fn open_and_short_are_faults() {
        assert_eq!(classify(0.0, 3.3), Err("open"));
        assert_eq!(classify(0.02, 3.3), Err("open"));
        assert_eq!(classify(3.29, 3.3), Err("shorted"));
        assert_eq!(classify(1.5, 3.3), Ok(1.5));
    }

    #[test]
    fn rejects_no_probes() {
        assert!(parse("").is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = parse(&ONE.replace("offset_c", "offset_f"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("offset_f"), "{err}");
        let err = parse(&format!("{ONE}\nresistor = 1.0\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("resistor"), "{err}");
    }

    #[test]
    fn rejects_vcc_channel_shared_with_a_probe() {
        let err = parse(&format!("vcc_channel = 0\n{ONE}"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("vcc_channel"), "{err}");
        assert!(parse(&format!("vcc_channel = 3\n{ONE}")).is_ok());
    }

    #[test]
    fn rejects_shared_channel() {
        let two = format!("{ONE}\n{}", ONE.replace("bath", "inlet"));
        let err = parse(&two).unwrap_err().to_string();
        assert!(err.contains("share channel A0"), "{err}");
    }

    #[test]
    fn rejects_channel_out_of_range() {
        assert!(parse(&ONE.replace("channel = 0", "channel = 4")).is_err());
    }
}
