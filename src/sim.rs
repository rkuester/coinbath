//! A simulated bath and a simulated miner, for running Coinbath
//! without hardware.
//!
//! `Bath` is a first-order thermal model. `run_bath` steps it on a
//! timer and feeds the state task the readings the probes would
//! send, heated by whatever share of full power the miner reports.
//! `run_miner` plays the miner: it holds whatever share the state
//! asks for and reports the telemetry the Mujina client would.

use std::time::Duration;

use anyhow::Result;

use crate::state::{Client, Command, Miner};

/// Room temperature the bath cools toward, in degrees Celsius.
const AMBIENT_C: f64 = 22.0;
/// Temperature rise above ambient at full power, once settled.
const FULL_POWER_RISE_C: f64 = 40.0;
/// Time constant of the bath, in seconds.
const TAU_S: f64 = 120.0;
/// Miner output at full power.
const FULL_POWER_W: f64 = 300.0;
const FULL_HASHRATE_HS: f64 = 30.0e12;
/// How far the chip runs above the water at full power. The rig
/// measured about 40 C at 150 W.
const CHIP_RISE_C: f64 = 40.0;
/// Where the simulated board caps its thread, as an EmberOne does:
/// full at the limit minus the band, nothing at the limit.
const CHIP_LIMIT_C: f64 = 75.0;
const CHIP_BAND_C: f64 = 10.0;
/// How often the simulator reports.
const TICK: Duration = Duration::from_secs(1);

/// A first-order model of water heated by a miner.
#[derive(Debug, Clone)]
pub struct Bath {
    pub bath_c: f64,
}

impl Bath {
    /// A bath at room temperature.
    pub fn new() -> Self {
        Self { bath_c: AMBIENT_C }
    }

    /// Advances the model by `dt` seconds with the miner at
    /// `power_fraction` of full power.
    pub fn step(&mut self, dt: f64, power_fraction: f64) {
        let target = AMBIENT_C + FULL_POWER_RISE_C * power_fraction.clamp(0.0, 1.0);
        self.bath_c += (target - self.bath_c) * (dt / TAU_S).min(1.0);
    }

    /// Water leaving the bath for the plates.
    pub fn inlet_c(&self) -> f64 {
        self.bath_c - 0.3
    }

    /// Water returning from the plates, warmer by the heat picked up.
    pub fn outlet_c(&self, power_fraction: f64) -> f64 {
        self.bath_c + 2.0 * power_fraction.clamp(0.0, 1.0)
    }
}

impl Default for Bath {
    fn default() -> Self {
        Self::new()
    }
}

/// Probe names the simulator reports, in index order.
pub const PROBE_NAMES: [&str; 3] = ["bath", "inlet", "outlet"];

/// Runs the simulated bath until the state task is gone.
///
/// The water is heated by the share of full power the miner
/// reports holding, so it works against the simulated miner and
/// against a real one alike. A miner that reports nothing is off.
pub async fn run_bath(client: Client) -> Result<()> {
    let mut bath = Bath::new();
    let mut ticker = tokio::time::interval(TICK);
    loop {
        ticker.tick().await;
        let power_fraction = client.state().miner.power_fraction.unwrap_or(0.0);
        bath.step(TICK.as_secs_f64(), power_fraction);

        let readings = [bath.bath_c, bath.inlet_c(), bath.outlet_c(power_fraction)];
        for (index, celsius) in readings.into_iter().enumerate() {
            client
                .send(Command::ProbeReading {
                    index,
                    volts: None,
                    celsius: Some(celsius),
                })
                .await?;
        }
    }
}

/// Runs the simulated miner until the state task is gone.
///
/// It holds whatever share of full power the state asks for, and
/// full power until something asks, as Mujina does, less whatever
/// its chip temperature caps it to. The chip follows the power at
/// once, so the cap settles within a tick.
pub async fn run_miner(client: Client) -> Result<()> {
    let mut ticker = tokio::time::interval(TICK);
    let mut held = 1.0_f64;
    loop {
        ticker.tick().await;
        let state = client.state();
        let asked = state.power_fraction.unwrap_or(1.0);
        let water_c = state.probes[0].celsius.unwrap_or(AMBIENT_C);
        let chip_c = water_c + CHIP_RISE_C * held;
        let ceiling = ((CHIP_LIMIT_C - chip_c) / CHIP_BAND_C).clamp(0.0, 1.0);
        held = asked.min(ceiling);
        client
            .send(Command::MinerReport(Miner {
                online: true,
                hashrate_hs: Some(FULL_HASHRATE_HS * held),
                power_w: Some(FULL_POWER_W * held),
                chip_temperature_c: Some(chip_c),
                power_fraction: Some(held),
                power_ceiling: Some(ceiling),
            }))
            .await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heats_under_power_and_cools_without() {
        let mut bath = Bath::new();
        bath.step(60.0, 1.0);
        assert!(bath.bath_c > AMBIENT_C);

        let warm = bath.bath_c;
        bath.step(60.0, 0.0);
        assert!(bath.bath_c < warm);
        assert!(bath.bath_c >= AMBIENT_C);
    }

    #[test]
    fn settles_below_full_power_rise() {
        let mut bath = Bath::new();
        for _ in 0..10_000 {
            bath.step(1.0, 1.0);
        }
        assert!((bath.bath_c - (AMBIENT_C + FULL_POWER_RISE_C)).abs() < 0.01);
        assert!(bath.outlet_c(1.0) > bath.inlet_c());
    }
}
