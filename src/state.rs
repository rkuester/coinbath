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

/// One chip on a board's chain, as the miner counts it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Chip {
    /// The chip's address on the chain.
    pub address: u64,
    /// Nonces it has reported since the thread started, and how
    /// many of them fell short of its own ticket mask.
    pub nonces: u64,
    pub hardware_errors: u64,
    /// Its rate over the last five minutes, from its nonces.
    pub hashrate_hs: Option<f64>,
}

/// One hash board, as the miner reports it. Every reading is None
/// when the board does not report it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    /// The miner's key for the board, its model and serial.
    pub name: String,
    /// Hash rate in hashes per second, summed over its threads.
    pub hashrate_hs: Option<f64>,
    /// The hottest chip on the board, in degrees Celsius.
    pub chip_temperature_c: Option<f64>,
    /// The core rail's output.
    pub voltage_v: Option<f64>,
    pub current_a: Option<f64>,
    pub power_w: Option<f64>,
    /// The core rail's input.
    pub input_voltage_v: Option<f64>,
    /// The core regulator's own temperature, in degrees Celsius.
    pub regulator_temperature_c: Option<f64>,
    /// The hottest board sensor, in degrees Celsius.
    pub board_temperature_c: Option<f64>,
    /// The share of full power the board's thread holds, and the
    /// most the board lets it hold. A ceiling under 1.0 means the
    /// board is throttling its thread by heat.
    pub power_fraction: Option<f64>,
    pub power_ceiling: Option<f64>,
    /// Its chips in chain order.
    pub chips: Vec<Chip>,
}

/// What the miner reports, as far as Coinbath cares. Every reading
/// is None until the miner reports it, and None again when it
/// stops.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Miner {
    /// Whether the last exchange with the miner succeeded.
    pub online: bool,
    /// Hash rate in hashes per second, summed over the miner.
    pub hashrate_hs: Option<f64>,
    /// Power draw in watts, summed over the boards that report it.
    pub power_w: Option<f64>,
    /// The hottest chip temperature any board reports, in degrees
    /// Celsius.
    pub chip_temperature_c: Option<f64>,
    /// The share of full power the miner holds, 0.0 to 1.0: the
    /// mean over its threads of what each holds, or what it was
    /// asked when no thread reports.
    pub power_fraction: Option<f64>,
    /// The lowest ceiling any thread reports, 0.0 to 1.0. Under
    /// 1.0 a board is throttling its thread by heat.
    pub power_ceiling: Option<f64>,
    /// Every board, in the miner's order.
    pub boards: Vec<Board>,
    /// From the miner's own document: how long it has run, the
    /// shares it has submitted, and the job source it works for
    /// with the difficulty that source set.
    pub uptime_secs: Option<u64>,
    pub shares_submitted: Option<u64>,
    pub pool: Option<String>,
    pub difficulty: Option<f64>,
}

/// Who decides the share of full power to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The control loop, from the bath temperature.
    Auto,
    /// Whoever last set it by hand.
    Manual,
}

/// The snapshot the state task publishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Target bath temperature in degrees Celsius.
    pub setpoint_c: f64,
    pub mode: Mode,
    /// The share of full power Coinbath asks of the miner, 0.0 to
    /// 1.0, or None while nothing has asked and the miner decides.
    /// The control loop sets it in automatic mode; a client that
    /// sets it puts the state in manual mode.
    pub power_fraction: Option<f64>,
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
            mode: Mode::Auto,
            power_fraction: None,
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
    /// Sets the share of full power to ask of the miner. From the
    /// control loop in automatic mode; from anyone else, it also
    /// switches to manual mode.
    SetPowerFraction {
        fraction: f64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Hands the power request to the control loop or takes it
    /// away.
    SetMode {
        mode: Mode,
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

    /// Sets the share of full power to ask of the miner and waits
    /// for the state task to accept or reject it. Leaves the mode
    /// alone: this is the control loop's call.
    pub async fn set_power_fraction(&self, fraction: f64) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.send(Command::SetPowerFraction { fraction, reply })
            .await?;
        response
            .await
            .map_err(|_| anyhow!("state task dropped the reply"))?
    }

    /// Sets the share of full power by hand, which also puts the
    /// state in manual mode so the control loop stays out of it.
    pub async fn set_power_fraction_by_hand(&self, fraction: f64) -> Result<()> {
        self.set_mode(Mode::Manual).await?;
        self.set_power_fraction(fraction).await
    }

    /// Sets the mode and waits for the state task to apply it.
    pub async fn set_mode(&self, mode: Mode) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.send(Command::SetMode { mode, reply }).await?;
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
        Command::SetPowerFraction { fraction, reply } => {
            let result = validate_power_fraction(fraction);
            let accepted = result.is_ok();
            if accepted {
                state.power_fraction = Some(fraction);
                tracing::info!(fraction, mode = ?state.mode, "power fraction changed");
            }
            let _ = reply.send(result);
            accepted
        }
        Command::SetMode { mode, reply } => {
            let changed = state.mode != mode;
            if changed {
                state.mode = mode;
                tracing::info!(?mode, "mode changed");
            }
            let _ = reply.send(Ok(()));
            changed
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

/// The miner's own range: 0.0 is off and 1.0 is full power.
const POWER_FRACTION_RANGE: std::ops::RangeInclusive<f64> = 0.0..=1.0;

fn validate_power_fraction(fraction: f64) -> Result<()> {
    if fraction.is_finite() && POWER_FRACTION_RANGE.contains(&fraction) {
        Ok(())
    } else {
        Err(Rejected(format!(
            "power fraction {fraction} is outside {}..={}",
            POWER_FRACTION_RANGE.start(),
            POWER_FRACTION_RANGE.end()
        ))
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn power_fraction_starts_unset_and_changes_on_request() {
        let (client, _task) = spawn(State::new(50.0, &["bath"]));
        let mut watcher = client.watch();
        assert_eq!(client.state().power_fraction, None);

        client.set_power_fraction(0.25).await.unwrap();

        watcher.changed().await.unwrap();
        assert_eq!(watcher.borrow().power_fraction, Some(0.25));
    }

    #[tokio::test]
    async fn a_hand_set_power_fraction_takes_manual_mode() {
        let (client, _task) = spawn(State::new(50.0, &["bath"]));
        assert_eq!(client.state().mode, Mode::Auto);

        client.set_power_fraction_by_hand(0.5).await.unwrap();
        let state = client.state();
        assert_eq!(state.mode, Mode::Manual);
        assert_eq!(state.power_fraction, Some(0.5));

        client.set_mode(Mode::Auto).await.unwrap();
        assert_eq!(client.state().mode, Mode::Auto);
        assert_eq!(client.state().power_fraction, Some(0.5));
    }

    #[tokio::test]
    async fn rejected_power_fraction_leaves_state_alone() {
        let (client, _task) = spawn(State::new(50.0, &["bath"]));

        let err = client.set_power_fraction(1.5).await.unwrap_err();
        assert!(err.downcast_ref::<Rejected>().is_some());
        assert!(client.set_power_fraction(-0.1).await.is_err());
        assert!(client.set_power_fraction(f64::NAN).await.is_err());

        assert_eq!(client.state().power_fraction, None);
    }

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
