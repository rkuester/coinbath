//! The state task and the snapshot it publishes.
//!
//! `State` is the snapshot every client reads. `Command` is the
//! only way a client changes it. `spawn` starts the task and hands
//! back a `Client`, which is cheap to clone and holds one end of
//! each channel.

use std::fmt;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

/// One temperature probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// Probe name, such as "bath", "inlet", or "outlet".
    pub name: String,
    /// Last reading in degrees Celsius, or None before the first
    /// or while the probe is open or shorted.
    pub celsius: Option<f64>,
    /// The divider voltage behind the last reading, or None when
    /// the source has no ADC.
    pub volts: Option<f64>,
}

/// What the miner reports, as far as Coinbath cares.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Miner {
    /// Hash rate in hashes per second.
    pub hashrate_hs: Option<f64>,
    /// Power draw in watts.
    pub power_w: Option<f64>,
    /// The share of full power Coinbath last asked for, 0.0 to 1.0.
    pub power_fraction: Option<f64>,
}

/// The snapshot the state task publishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Target bath temperature in degrees Celsius.
    pub setpoint_c: f64,
    pub probes: Vec<Probe>,
    /// The divider supply as last measured, in volts.
    pub supply_v: Option<f64>,
    pub miner: Miner,
}

impl State {
    /// Builds a state with the given setpoint and probe names, and
    /// no readings.
    pub fn new(setpoint_c: f64, probe_names: &[&str]) -> Self {
        Self {
            setpoint_c,
            probes: probe_names
                .iter()
                .map(|name| Probe {
                    name: name.to_string(),
                    celsius: None,
                    volts: None,
                })
                .collect(),
            supply_v: None,
            miner: Miner::default(),
        }
    }
}

/// A command the state task refused, with the reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected(pub String);

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

/// A change a client asks the state task to make.
#[derive(Debug)]
pub enum Command {
    /// Sets the target bath temperature.
    SetSetpoint {
        celsius: f64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Records a probe reading, by probe index. `celsius` is None
    /// when the probe is open or shorted; `volts` is None when the
    /// source has no ADC.
    ProbeReading {
        index: usize,
        volts: Option<f64>,
        celsius: Option<f64>,
    },
    /// Records the measured divider supply.
    Supply { volts: f64 },
    /// Records what the miner last reported.
    MinerReport(Miner),
}

/// A client's handle on the state task.
#[derive(Debug, Clone)]
pub struct Client {
    state: watch::Receiver<State>,
    commands: mpsc::Sender<Command>,
}

impl Client {
    /// Returns the current snapshot.
    pub fn state(&self) -> State {
        self.state.borrow().clone()
    }

    /// Returns a receiver that wakes on every published change.
    pub fn watch(&self) -> watch::Receiver<State> {
        self.state.clone()
    }

    /// Sets the target bath temperature and waits for the state
    /// task to accept or reject it.
    pub async fn set_setpoint(&self, celsius: f64) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.send(Command::SetSetpoint { celsius, reply }).await?;
        response
            .await
            .map_err(|_| anyhow!("state task dropped the reply"))?
    }

    /// Sends a command without waiting for a reply.
    pub async fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| anyhow!("state task is gone"))
    }
}

/// Starts the state task with `initial` as its first snapshot.
pub fn spawn(initial: State) -> (Client, JoinHandle<()>) {
    let (state_tx, state_rx) = watch::channel(initial);
    let (command_tx, command_rx) = mpsc::channel(32);
    let task = tokio::spawn(run(state_tx, command_rx));
    let client = Client {
        state: state_rx,
        commands: command_tx,
    };
    (client, task)
}

async fn run(state: watch::Sender<State>, mut commands: mpsc::Receiver<Command>) {
    while let Some(command) = commands.recv().await {
        state.send_if_modified(|state| apply(state, command));
    }
    tracing::info!("state task stopping: every client is gone");
}

/// Applies one command and reports whether the state changed.
fn apply(state: &mut State, command: Command) -> bool {
    match command {
        Command::SetSetpoint { celsius, reply } => {
            let result = validate_setpoint(celsius);
            let accepted = result.is_ok();
            if accepted {
                state.setpoint_c = celsius;
                tracing::info!(celsius, "setpoint changed");
            }
            // A client that gave up waiting is not an error here.
            let _ = reply.send(result);
            accepted
        }
        Command::ProbeReading {
            index,
            volts,
            celsius,
        } => match state.probes.get_mut(index) {
            Some(probe) => {
                probe.celsius = celsius;
                probe.volts = volts;
                true
            }
            None => {
                tracing::warn!(index, "reading for a probe that does not exist");
                false
            }
        },
        Command::Supply { volts } => {
            state.supply_v = Some(volts);
            true
        }
        Command::MinerReport(miner) => {
            state.miner = miner;
            true
        }
    }
}

/// Bath temperatures outside this range are a client's mistake, and
/// the top is where a sous vide bath stops being one.
const SETPOINT_RANGE_C: std::ops::RangeInclusive<f64> = 0.0..=95.0;

fn validate_setpoint(celsius: f64) -> Result<()> {
    if celsius.is_finite() && SETPOINT_RANGE_C.contains(&celsius) {
        Ok(())
    } else {
        Err(Rejected(format!(
            "setpoint {celsius} C is outside {}..={} C",
            SETPOINT_RANGE_C.start(),
            SETPOINT_RANGE_C.end()
        ))
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn setpoint_change_reaches_every_watcher() {
        let (client, _task) = spawn(State::new(50.0, &["bath"]));
        let mut watcher = client.watch();

        client.set_setpoint(60.0).await.unwrap();

        watcher.changed().await.unwrap();
        assert_eq!(watcher.borrow().setpoint_c, 60.0);
        assert_eq!(client.state().setpoint_c, 60.0);
    }

    #[tokio::test]
    async fn rejected_setpoint_leaves_state_alone() {
        let (client, _task) = spawn(State::new(50.0, &["bath"]));

        let err = client.set_setpoint(150.0).await.unwrap_err();
        assert!(err.downcast_ref::<Rejected>().is_some());
        assert!(client.set_setpoint(f64::NAN).await.is_err());

        assert_eq!(client.state().setpoint_c, 50.0);
    }

    #[tokio::test]
    async fn readings_fill_in_by_index() {
        let (client, _task) = spawn(State::new(50.0, &["bath", "inlet"]));
        let mut watcher = client.watch();

        client
            .send(Command::ProbeReading {
                index: 1,
                volts: Some(1.5),
                celsius: Some(48.5),
            })
            .await
            .unwrap();

        watcher.changed().await.unwrap();
        let state = watcher.borrow();
        assert_eq!(state.probes[0].celsius, None);
        assert_eq!(state.probes[1].celsius, Some(48.5));
        assert_eq!(state.probes[1].volts, Some(1.5));
    }

    #[tokio::test]
    async fn task_stops_when_the_last_client_drops() {
        let (client, task) = spawn(State::new(50.0, &[]));
        drop(client);
        task.await.unwrap();
    }
}
