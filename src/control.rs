//! The control loop, a client that turns the bath's temperature
//! error into a share of the miner's full power.
//!
//! `Controller` is the arithmetic, a proportional-integral law
//! with anti-windup, stepped once per period. `run` reads the bath
//! probe from the snapshot, steps the controller, and asks the
//! state task for the share, while the state is in automatic mode.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::state::{Client, Mode, State};

/// How often the loop steps. Water moves slowly; the miner's
/// telemetry arrives every two seconds.
pub const PERIOD: Duration = Duration::from_secs(2);

/// Time constant of the filter on the bath reading, in seconds.
/// The water moves a quarter degree a minute at full power, so
/// half a minute of smoothing lags it by an eighth of a degree and
/// takes the probe's remaining noise out of the request.
const BATH_FILTER_S: f64 = 30.0;

/// A request within this much of the last one is not sent, since
/// every change makes the miner re-plan its frequency. At the
/// default gain this is six hundredths of a degree.
const REQUEST_DEADBAND: f64 = 0.03;

/// Tuning, from the configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Gains {
    /// Share of full power per degree Celsius of error.
    pub kp: f64,
    /// Share of full power per degree Celsius of accumulated
    /// error per second.
    pub ki: f64,
}

impl Default for Gains {
    /// Full power from two degrees below the setpoint, and an
    /// integral that takes about ten minutes to match the
    /// proportional term at a steady one-degree error. The rig's
    /// bath is about 38 kJ/K and heats 0.24 C/min at 150 W, so a
    /// tighter band would hunt.
    fn default() -> Self {
        Self {
            kp: 0.5,
            ki: 0.0008,
        }
    }
}

/// A proportional-integral controller on temperature.
#[derive(Debug, Clone)]
pub struct Controller {
    gains: Gains,
    /// Accumulated error, in degree-seconds.
    integral: f64,
    /// The bath reading after the filter, or None before the first.
    filtered_c: Option<f64>,
}

impl Controller {
    pub fn new(gains: Gains) -> Self {
        Self {
            gains,
            integral: 0.0,
            filtered_c: None,
        }
    }

    /// Passes a bath reading through the filter and returns the
    /// smoothed value. The first reading seeds the filter.
    pub fn filter(&mut self, bath_c: f64, dt: f64) -> f64 {
        let filtered = match self.filtered_c {
            Some(f) => f + (bath_c - f) * (dt / BATH_FILTER_S).min(1.0),
            None => bath_c,
        };
        self.filtered_c = Some(filtered);
        filtered
    }

    /// Steps the controller by `dt` seconds and returns the share
    /// of full power, 0.0 to 1.0.
    ///
    /// The integral holds still while the output is saturated in
    /// the direction the error pushes, so a long heat-up from cold
    /// does not overshoot when the bath arrives.
    pub fn step(&mut self, setpoint_c: f64, bath_c: f64, dt: f64) -> f64 {
        let error = setpoint_c - bath_c;
        let proposed = self.gains.kp * error + self.gains.ki * (self.integral + error * dt);
        let saturated_high = proposed > 1.0 && error > 0.0;
        let saturated_low = proposed < 0.0 && error < 0.0;
        if !saturated_high && !saturated_low {
            self.integral += error * dt;
        }
        (self.gains.kp * error + self.gains.ki * self.integral).clamp(0.0, 1.0)
    }

    /// Forgets the accumulated error and the filter, for when the
    /// loop takes over after a spell of manual control.
    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.filtered_c = None;
    }
}

/// Runs the loop until the state task is gone.
///
/// In automatic mode, every period with a bath reading ends in a
/// request to the state task. Manual mode leaves the request to
/// whoever set it, and the controller starts afresh when automatic
/// mode returns.
pub async fn run(client: Client, gains: Gains) -> Result<()> {
    let mut controller = Controller::new(gains);
    let mut ticker = tokio::time::interval(PERIOD);
    let mut was_auto = true;
    loop {
        ticker.tick().await;
        let state = client.state();
        if state.mode != Mode::Auto {
            was_auto = false;
            continue;
        }
        if !was_auto {
            controller.reset();
            was_auto = true;
        }
        let Some(bath_c) = bath_reading(&state) else {
            continue;
        };
        let dt = PERIOD.as_secs_f64();
        let bath_c = controller.filter(bath_c, dt);
        let fraction = controller.step(state.setpoint_c, bath_c, dt);
        // A small change is not worth a frequency re-plan, except at
        // the ends of the range, which the miner treats as states.
        let worth_sending = match state.power_fraction {
            Some(last) => {
                (fraction - last).abs() >= REQUEST_DEADBAND
                    || (fraction == 0.0 && last != 0.0)
                    || (fraction == 1.0 && last != 1.0)
            }
            None => true,
        };
        if worth_sending {
            client
                .set_power_fraction(fraction)
                .await
                .context("control loop request")?;
        }
    }
}

/// The bath probe's reading, by name.
pub fn bath_reading(state: &State) -> Option<f64> {
    state
        .probes
        .iter()
        .find(|p| p.name == "bath")
        .and_then(|p| p.celsius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_power_when_cold_and_off_when_hot() {
        let mut c = Controller::new(Gains::default());
        assert_eq!(c.step(50.0, 20.0, 2.0), 1.0);
        assert_eq!(c.step(50.0, 60.0, 2.0), 0.0);
    }

    #[test]
    fn proportional_between() {
        let mut c = Controller::new(Gains { kp: 0.5, ki: 0.0 });
        assert!((c.step(50.0, 49.0, 2.0) - 0.5).abs() < 1e-9);
        assert!((c.step(50.0, 49.5, 2.0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn integral_lifts_a_steady_error() {
        let mut c = Controller::new(Gains::default());
        let first = c.step(50.0, 49.0, 2.0);
        let mut last = first;
        for _ in 0..300 {
            last = c.step(50.0, 49.0, 2.0);
        }
        assert!(last > first, "{last} > {first}");
        assert!(last <= 1.0);
    }

    #[test]
    fn integral_holds_while_saturated() {
        let mut c = Controller::new(Gains::default());
        for _ in 0..1000 {
            c.step(50.0, 20.0, 2.0);
        }
        // A long heat-up left nothing behind: at the setpoint the
        // output is the proportional term alone, which is zero.
        assert_eq!(c.step(50.0, 50.0, 2.0), 0.0);
    }

    #[test]
    fn the_filter_seeds_on_the_first_reading_and_smooths_the_rest() {
        let mut c = Controller::new(Gains::default());
        assert_eq!(c.filter(40.0, 2.0), 40.0);
        let next = c.filter(41.0, 2.0);
        assert!(next > 40.0 && next < 40.5, "{next}");
        for _ in 0..100 {
            c.filter(41.0, 2.0);
        }
        assert!((c.filter(41.0, 2.0) - 41.0).abs() < 0.01);
        c.reset();
        assert_eq!(c.filter(30.0, 2.0), 30.0);
    }

    #[test]
    fn reset_forgets_the_integral() {
        let mut c = Controller::new(Gains::default());
        for _ in 0..100 {
            c.step(50.0, 49.0, 2.0);
        }
        c.reset();
        assert_eq!(c.step(50.0, 50.0, 2.0), 0.0);
    }

    #[test]
    fn holds_the_simulated_bath_at_the_setpoint() {
        let mut bath = crate::sim::Bath::new();
        let mut c = Controller::new(Gains::default());
        let dt = 2.0;
        let mut fraction = 0.0;
        for _ in 0..3600 {
            bath.step(dt, fraction);
            fraction = c.step(45.0, bath.bath_c, dt);
        }
        assert!(
            (bath.bath_c - 45.0).abs() < 0.2,
            "settled at {}",
            bath.bath_c
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn asks_only_in_auto_mode() {
        use crate::state::{self, Command};

        let (client, _task) = state::spawn(State::new(50.0, &["bath"]));
        let _loop = tokio::spawn(run(client.clone(), Gains::default()));
        let mut watcher = client.watch();

        client
            .send(Command::ProbeReading {
                index: 0,
                volts: None,
                celsius: Some(20.0),
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                watcher.changed().await.unwrap();
                if watcher.borrow_and_update().power_fraction == Some(1.0) {
                    break;
                }
            }
        })
        .await
        .expect("the loop asks for full power from cold");

        client.set_mode(Mode::Manual).await.unwrap();
        client.set_power_fraction(0.1).await.unwrap();
        tokio::time::sleep(PERIOD * 2).await;
        assert_eq!(client.state().power_fraction, Some(0.1));
    }
}
