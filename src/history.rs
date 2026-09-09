//! Recent readings for the sparklines, sampled from the snapshot.
//!
//! `History` is a ring of optional values, oldest first when read,
//! with a gap where a reading was missing. `Histories` holds one
//! per sparkline and samples them all from a snapshot.

use std::collections::VecDeque;
use std::time::Duration;

use crate::control;
use crate::state::State;

/// How often the histories are sampled.
pub const SAMPLE: Duration = Duration::from_secs(2);

/// Samples kept per history. With one every two seconds, ten
/// minutes, which is a demo session.
pub const CAPACITY: usize = 300;

/// A ring of the last `CAPACITY` samples.
#[derive(Debug, Clone, Default)]
pub struct History(VecDeque<Option<f64>>);

impl History {
    pub fn push(&mut self, value: Option<f64>) {
        if self.0.len() == CAPACITY {
            self.0.pop_front();
        }
        self.0.push_back(value);
    }

    /// Samples oldest first, gaps included.
    pub fn samples(&self) -> Vec<Option<f64>> {
        self.0.iter().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The span the samples cover, in seconds.
    pub fn span_secs(&self) -> f64 {
        self.0.len() as f64 * SAMPLE.as_secs_f64()
    }
}

/// One history per sparkline on the display.
#[derive(Debug, Clone, Default)]
pub struct Histories {
    pub bath: History,
    pub inlet: History,
    pub outlet: History,
    pub hashrate_ths: History,
    pub power_w: History,
    pub chip_c: History,
}

impl Histories {
    /// Appends one sample of each from the snapshot.
    pub fn sample(&mut self, state: &State) {
        self.bath.push(control::bath_reading(state));
        self.inlet.push(probe(state, "inlet"));
        self.outlet.push(probe(state, "outlet"));
        self.hashrate_ths
            .push(state.miner.hashrate_hs.map(|h| h / 1e12));
        self.power_w.push(state.miner.power_w);
        self.chip_c.push(state.miner.chip_temperature_c);
    }
}

fn probe(state: &State, name: &str) -> Option<f64> {
    state
        .probes
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| p.celsius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_latest_samples_in_order() {
        let mut h = History::default();
        for i in 0..(CAPACITY + 5) {
            h.push(Some(i as f64));
        }
        let samples = h.samples();
        assert_eq!(samples.len(), CAPACITY);
        assert_eq!(samples[0], Some(5.0));
        assert_eq!(*samples.last().unwrap(), Some((CAPACITY + 4) as f64));
    }

    #[test]
    fn a_missing_reading_is_a_gap() {
        let mut state = State::new(50.0, &["bath", "inlet", "outlet"]);
        let mut histories = Histories::default();
        histories.sample(&state);
        state.probes[0].celsius = Some(40.0);
        state.miner.hashrate_hs = Some(7.5e12);
        histories.sample(&state);

        assert_eq!(histories.bath.samples(), [None, Some(40.0)]);
        assert_eq!(histories.hashrate_ths.samples(), [None, Some(7.5)]);
        assert_eq!(histories.inlet.samples(), [None, None]);
        assert_eq!(histories.bath.span_secs(), 4.0);
    }
}
