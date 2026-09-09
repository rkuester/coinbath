//! Coinbath holds a sous vide bath at temperature with the waste
//! heat of a Bitcoin miner.
//!
//! One state task owns the setpoint, the probe readings, and the
//! miner telemetry, and publishes a snapshot of them over a watch
//! channel. Every client, the display, the HTTP API, and the
//! command line, reads that snapshot and sends changes through the
//! state task's command channel.

pub mod adc;
pub mod api;
pub mod calibration;
pub mod config;
pub mod control;
pub mod mujina;
pub mod probes;
pub mod sim;
pub mod state;
pub mod thermistor;
