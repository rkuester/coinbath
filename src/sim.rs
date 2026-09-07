//! A simulated bath, for running Coinbath without hardware.
//!
//! `Bath` is a first-order thermal model. `run` steps it on a timer
//! and feeds the state task the readings a real rig would send.

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

/// Runs the simulator until the state task is gone.
///
/// The miner runs at whatever power fraction the state last asked
/// for, and at full power until something asks.
pub async fn run(client: Client) -> Result<()> {
    let mut bath = Bath::new();
    let mut ticker = tokio::time::interval(TICK);
    loop {
        ticker.tick().await;
        let power_fraction = client.state().miner.power_fraction.unwrap_or(1.0);
        bath.step(TICK.as_secs_f64(), power_fraction);

        let readings = [bath.bath_c, bath.inlet_c(), bath.outlet_c(power_fraction)];
        for (index, celsius) in readings.into_iter().enumerate() {
            client
                .send(Command::ProbeReading { index, celsius })
                .await?;
        }
        client
            .send(Command::MinerReport(Miner {
                hashrate_hs: Some(FULL_HASHRATE_HS * power_fraction),
                power_w: Some(FULL_POWER_W * power_fraction),
                power_fraction: Some(power_fraction),
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
