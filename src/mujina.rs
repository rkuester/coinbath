//! The Mujina client, a source that feeds the state task from the
//! miner's REST API and carries Coinbath's power request to it.
//!
//! Every poll reads the whole tree at `GET /api/v0` and reports what
//! Coinbath cares about: the hash rate summed over threads, the
//! power summed over regulators, the hottest chip, the share of
//! full power the threads hold, and the lowest ceiling a board puts
//! on its thread. Whenever the state's requested share
//! differs from what the miner was last told, the client writes it
//! to `PUT /api/v0/target_power_fraction`, and it writes it again
//! after the miner comes back from an outage, since a restarted
//! miner forgets.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::task;

use crate::state::{Client, Command, Miner};

/// Where the miner's API is by default. Mujina binds to loopback
/// on port 7785 unless told otherwise.
pub const DEFAULT_URL: &str = "http://127.0.0.1:7785";

/// How often the miner is polled.
const POLL: Duration = Duration::from_secs(2);

/// Longer than this and the miner is as good as gone for this poll.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Runs the client until the state task is gone.
///
/// A miner that stops answering is logged once, reported as offline
/// with no readings, and polled until it answers again.
pub async fn run(client: Client, url: String) -> Result<()> {
    let api = Api::new(url);
    let mut watcher = client.watch();
    let mut ticker = tokio::time::interval(POLL);
    // The share the miner holds because this client wrote it. None
    // until the first write and after any failed exchange, so the
    // next poll writes the request again.
    let mut asserted: Option<f64> = None;
    let mut requested: Option<f64> = None;
    let mut was_online = true;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            changed = watcher.changed() => {
                changed.context("state task is gone")?;
                if watcher.borrow_and_update().power_fraction == requested {
                    continue;
                }
            }
        }
        requested = client.state().power_fraction;

        let exchange: Result<Miner> = async {
            if let Some(fraction) = requested
                && asserted != Some(fraction)
            {
                api.put_power_fraction(fraction).await?;
                asserted = Some(fraction);
                tracing::info!(fraction, "asked the miner for a share of full power");
            }
            let tree = api.get_tree().await?;
            Ok(summarize(&tree))
        }
        .await;

        let report = match exchange {
            Ok(report) => {
                if !was_online {
                    tracing::info!("miner answering again");
                    was_online = true;
                }
                report
            }
            Err(e) => {
                if was_online {
                    tracing::warn!("miner not answering: {e:#}");
                    was_online = false;
                }
                asserted = None;
                Miner::default()
            }
        };
        client.send(Command::MinerReport(report)).await?;
    }
}

/// Reduces the miner's tree to what Coinbath cares about.
///
/// Hash rate is the sum over every thread of every board. Power is
/// the sum over every regulator that reports it. The chip
/// temperature is the hottest a board reports, under whichever name
/// the board uses for it. The share held is the mean over the
/// threads that report one, else what the miner was asked; the
/// ceiling is the lowest any thread reports. A reading is None
/// when no board reports one.
pub fn summarize(tree: &Value) -> Miner {
    let mut hashrate = Sum::default();
    let mut power = Sum::default();
    let mut chip = Max::default();
    let mut held = Mean::default();
    let mut ceiling = Min::default();
    for board in objects(tree.get("boards")) {
        for thread in objects(board.get("threads")) {
            hashrate.add(number(thread, "hashes_per_s"));
            held.add(number(thread, "power_fraction"));
            ceiling.add(number(thread, "power_ceiling"));
        }
        for regulator in objects(board.get("regulators")) {
            power.add(number(regulator, "power_w"));
        }
        for (name, value) in board.as_object().into_iter().flatten() {
            if is_chip_temperature(name) {
                chip.add(value.as_f64());
            }
        }
    }
    let asked = tree.get("target_power_fraction").and_then(Value::as_f64);
    Miner {
        online: true,
        hashrate_hs: hashrate.total(),
        power_w: power.total(),
        chip_temperature_c: chip.max(),
        power_fraction: held.mean().or(asked),
        power_ceiling: ceiling.min(),
    }
}

/// An EmberOne reports `chip_0_temperature_c` and a Bitaxe reports
/// `asic_temperature_c`. Both are the die.
fn is_chip_temperature(name: &str) -> bool {
    name.ends_with("_temperature_c") && (name.starts_with("chip_") || name.starts_with("asic_"))
}

/// The values of a JSON object, or nothing for anything else.
fn objects(value: Option<&Value>) -> impl Iterator<Item = &Value> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|map| map.values())
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

/// A sum that stays None until something is added.
#[derive(Default)]
struct Sum(Option<f64>);

impl Sum {
    fn add(&mut self, value: Option<f64>) {
        if let Some(v) = value {
            self.0 = Some(self.0.unwrap_or(0.0) + v);
        }
    }

    fn total(&self) -> Option<f64> {
        self.0
    }
}

/// A mean that stays None until something is added.
#[derive(Default)]
struct Mean(Option<(f64, usize)>);

impl Mean {
    fn add(&mut self, value: Option<f64>) {
        if let Some(v) = value {
            let (sum, n) = self.0.unwrap_or((0.0, 0));
            self.0 = Some((sum + v, n + 1));
        }
    }

    fn mean(&self) -> Option<f64> {
        self.0.map(|(sum, n)| sum / n as f64)
    }
}

/// A minimum that stays None until something is added.
#[derive(Default)]
struct Min(Option<f64>);

impl Min {
    fn add(&mut self, value: Option<f64>) {
        if let Some(v) = value {
            self.0 = Some(self.0.map_or(v, |m| m.min(v)));
        }
    }

    fn min(&self) -> Option<f64> {
        self.0
    }
}

/// A maximum that stays None until something is added.
#[derive(Default)]
struct Max(Option<f64>);

impl Max {
    fn add(&mut self, value: Option<f64>) {
        if let Some(v) = value {
            self.0 = Some(self.0.map_or(v, |m| m.max(v)));
        }
    }

    fn max(&self) -> Option<f64> {
        self.0
    }
}

/// The two requests the client makes, on a blocking HTTP agent run
/// off the async runtime.
#[derive(Clone)]
struct Api {
    agent: ureq::Agent,
    url: String,
}

impl Api {
    fn new(url: String) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent, url }
    }

    async fn get_tree(&self) -> Result<Value> {
        let api = self.clone();
        task::spawn_blocking(move || {
            let mut response = api
                .agent
                .get(format!("{}/api/v0", api.url))
                .call()
                .context("get the tree")?;
            let status = response.status();
            if !status.is_success() {
                bail!("get the tree: {status}");
            }
            response.body_mut().read_json().context("parse the tree")
        })
        .await
        .context("tree request panicked")?
    }

    async fn put_power_fraction(&self, fraction: f64) -> Result<()> {
        let api = self.clone();
        task::spawn_blocking(move || {
            let mut response = api
                .agent
                .put(format!("{}/api/v0/target_power_fraction", api.url))
                .send_json(fraction)
                .context("put the power fraction")?;
            let status = response.status();
            if !status.is_success() {
                let body = response.body_mut().read_to_string().unwrap_or_default();
                bail!("put the power fraction: {status}: {body}");
            }
            Ok(())
        })
        .await
        .context("power fraction request panicked")?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State as Shared;
    use axum::http::StatusCode;
    use axum::routing::{get, put};
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;
    use crate::state::{self, State};

    /// Two EmberOnes and a CPU miner, in the shapes their drivers
    /// publish. The second EmberOne's regulator reads nothing and
    /// its chip is the hotter one.
    fn rig() -> Value {
        json!({
            "target_power_fraction": 0.5,
            "boards": {
                "emberone-00-2d714701": {
                    "model": "emberOne/00",
                    "status": "present",
                    "pcb_left_temperature_c": 41.0,
                    "chip_0_temperature_c": 62.5,
                    "regulators": {"core": {"voltage_v": 1.2, "power_w": 27.0}},
                    "threads": {"0": {
                        "hashes_per_s": 4.0e12, "status": "present",
                        "power_fraction": 0.5, "power_ceiling": 1.0
                    }}
                },
                "emberone-00-328679e0": {
                    "model": "emberOne/00",
                    "status": "present",
                    "chip_0_temperature_c": 71.0,
                    "regulators": {"core": {"voltage_v": null, "power_w": null}},
                    "threads": {"0": {
                        "hashes_per_s": 3.5e12,
                        "power_fraction": 0.3, "power_ceiling": 0.3
                    }}
                },
                "cpu-miner-0": {
                    "model": "CPU Miner",
                    "status": "present",
                    "threads": {
                        "0": {"hashes_per_s": 39993, "target_duty_percent": 10.0},
                        "1": {"hashes_per_s": 39994, "target_duty_percent": 10.0}
                    }
                }
            }
        })
    }

    #[test]
    fn sums_threads_and_regulators_and_takes_the_hottest_chip() {
        let miner = summarize(&rig());
        assert!(miner.online);
        assert_eq!(miner.hashrate_hs, Some(7.5e12 + 79987.0));
        assert_eq!(miner.power_w, Some(27.0));
        assert_eq!(miner.chip_temperature_c, Some(71.0));
        assert_eq!(
            miner.power_fraction,
            Some(0.4),
            "the mean of what the threads hold"
        );
        assert_eq!(miner.power_ceiling, Some(0.3), "the hot board's ceiling");
    }

    #[test]
    fn without_thread_reports_the_ask_stands_in_for_what_is_held() {
        let tree = json!({"target_power_fraction": 0.7, "boards": {"cpu-miner-0": {
            "threads": {"0": {"hashes_per_s": 1}}
        }}});
        let miner = summarize(&tree);
        assert_eq!(miner.power_fraction, Some(0.7));
        assert_eq!(miner.power_ceiling, None);
    }

    #[test]
    fn a_bitaxe_die_counts_as_a_chip() {
        let tree = json!({"boards": {"bitaxe-gamma-e2f5": {
            "asic_temperature_c": 55.0,
            "pcb_temperature_c": 80.0
        }}});
        assert_eq!(summarize(&tree).chip_temperature_c, Some(55.0));
    }

    #[test]
    fn nothing_reported_is_none_not_zero() {
        let miner = summarize(&json!({"boards": {}}));
        assert_eq!(miner.hashrate_hs, None);
        assert_eq!(miner.power_w, None);
        assert_eq!(miner.chip_temperature_c, None);
        assert_eq!(miner.power_fraction, None);
        assert_eq!(miner.power_ceiling, None);
        assert_eq!(summarize(&json!(null)).hashrate_hs, None);
    }

    /// A stand-in for Mujina that records what it is told and can
    /// be made to fail every request.
    #[derive(Default)]
    struct Fake {
        puts: Vec<f64>,
        down: bool,
    }

    type Handle = Arc<Mutex<Fake>>;

    async fn serve_fake() -> (String, Handle) {
        let fake: Handle = Arc::default();
        let router = Router::new()
            .route("/api/v0", get(get_tree))
            .route("/api/v0/target_power_fraction", put(put_fraction))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{addr}"), fake)
    }

    async fn get_tree(Shared(fake): Shared<Handle>) -> Result<Json<Value>, StatusCode> {
        let fake = fake.lock().unwrap();
        if fake.down {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let mut tree = rig();
        let asked = fake.puts.last().copied().unwrap_or(1.0);
        tree["target_power_fraction"] = json!(asked);
        // Every thread holds what was asked, uncapped.
        for board in tree["boards"].as_object_mut().unwrap().values_mut() {
            for thread in board["threads"].as_object_mut().unwrap().values_mut() {
                thread["power_fraction"] = json!(asked);
                thread["power_ceiling"] = json!(1.0);
            }
        }
        Ok(Json(tree))
    }

    async fn put_fraction(
        Shared(fake): Shared<Handle>,
        Json(fraction): Json<f64>,
    ) -> Result<Json<f64>, StatusCode> {
        let mut fake = fake.lock().unwrap();
        if fake.down {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        fake.puts.push(fraction);
        Ok(Json(fraction))
    }

    /// Waits for a miner report the predicate accepts.
    async fn report_where(
        watcher: &mut tokio::sync::watch::Receiver<State>,
        accept: impl Fn(&Miner) -> bool,
    ) -> Miner {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                watcher.changed().await.unwrap();
                let miner = watcher.borrow_and_update().miner.clone();
                if accept(&miner) {
                    return miner;
                }
            }
        })
        .await
        .expect("a matching report within the timeout")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_the_tree_and_writes_the_request_once() {
        let (base, fake) = serve_fake().await;
        let (client, _task) = state::spawn(State::new(50.0, &[]));
        let mut watcher = client.watch();
        let _source = tokio::spawn(run(client.clone(), base));

        let miner = report_where(&mut watcher, |m| m.online).await;
        assert_eq!(miner.power_w, Some(27.0));
        assert_eq!(miner.power_fraction, Some(1.0));
        assert!(fake.lock().unwrap().puts.is_empty(), "nothing asked yet");

        client.set_power_fraction(0.25).await.unwrap();
        let miner = report_where(&mut watcher, |m| m.power_fraction == Some(0.25)).await;
        assert!(miner.online);
        // Two more polls without a new request write nothing.
        report_where(&mut watcher, |m| m.online).await;
        report_where(&mut watcher, |m| m.online).await;
        assert_eq!(fake.lock().unwrap().puts, [0.25]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_outage_reads_as_offline_and_the_request_is_written_again() {
        let (base, fake) = serve_fake().await;
        let (client, _task) = state::spawn(State::new(50.0, &[]));
        let mut watcher = client.watch();
        let _source = tokio::spawn(run(client.clone(), base));

        client.set_power_fraction(0.4).await.unwrap();
        // The held share is a mean over threads, so it rounds.
        report_where(&mut watcher, |m| {
            m.power_fraction.is_some_and(|f| (f - 0.4).abs() < 1e-9)
        })
        .await;

        fake.lock().unwrap().down = true;
        let miner = report_where(&mut watcher, |m| !m.online).await;
        assert_eq!(miner, Miner::default());

        fake.lock().unwrap().down = false;
        report_where(&mut watcher, |m| m.online).await;
        assert_eq!(fake.lock().unwrap().puts, [0.4, 0.4]);
    }
}
