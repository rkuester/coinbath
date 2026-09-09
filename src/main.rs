use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

use coinbath::config::Config;
use coinbath::state::{self, Client, State};
use coinbath::{api, mujina, probes, sim};

/// The coinbath daemon.
#[derive(Parser)]
#[command(name = "coinbath", args_override_self = true)]
struct Args {
    /// Configuration file.
    #[arg(long, env = "COINBATH_CONFIG", default_value = "/etc/coinbath.toml")]
    config: PathBuf,

    /// Simulate hardware instead of using it: the bath and the
    /// miner, or the bath alone against a real Mujina.
    #[arg(long, value_name = "WHAT", num_args = 0..=1, default_missing_value = "all")]
    sim: Option<Sim>,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Sim {
    /// A simulated bath heated by a simulated miner.
    All,
    /// A simulated bath heated by the miner Mujina reports.
    Bath,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let config = Config::load(&args.config)?;
    let sim_bath = args.sim.is_some();
    let sim_miner = args.sim == Some(Sim::All);
    tracing::info!(
        setpoint_c = config.setpoint_c,
        api_listen = %config.api_listen,
        mujina_url = %config.mujina_url,
        probes = config.probes.probes.len(),
        sim_bath,
        sim_miner,
        "loaded {}",
        args.config.display()
    );

    let probe_names: Vec<&str> = if sim_bath {
        sim::PROBE_NAMES.to_vec()
    } else {
        config.probes.validate().context("probes")?;
        config.probes.names()
    };
    let (client, state_task) = state::spawn(State::new(config.setpoint_c, &probe_names));

    let mut probes = if sim_bath {
        tokio::spawn(sim::run_bath(client.clone()))
    } else {
        tokio::spawn(probes::run(client.clone(), config.probes.clone()))
    };
    let mut miner = if sim_miner {
        tokio::spawn(sim::run_miner(client.clone()))
    } else {
        tokio::spawn(mujina::run(client.clone(), config.mujina_url.clone()))
    };
    let mut api = tokio::spawn(api::serve(client.clone(), config.api_listen));
    let logger = tokio::spawn(log_changes(client.clone()));

    // The sources and the API run until shutdown; any one ending
    // early is a failure the operator must see.
    let outcome = tokio::select! {
        r = wait_for_shutdown() => r,
        r = &mut probes => Err(stopped_early("probes", r)),
        r = &mut miner => Err(stopped_early("miner client", r)),
        r = &mut api => Err(stopped_early("API", r)),
    };
    tracing::info!("shutting down");

    probes.abort();
    miner.abort();
    api.abort();
    logger.abort();
    drop(client);
    state_task.await.context("state task panicked")?;
    outcome
}

fn stopped_early(what: &str, result: Result<Result<()>, tokio::task::JoinError>) -> anyhow::Error {
    match result {
        Ok(Ok(())) => anyhow::anyhow!("{what} stopped"),
        Ok(Err(e)) => e.context(format!("{what} stopped")),
        Err(e) => anyhow::anyhow!("{what} panicked: {e}"),
    }
}

/// Logs the snapshot after each change, at most once a second.
async fn log_changes(client: Client) {
    let mut watcher = client.watch();
    let mut throttle = tokio::time::interval(Duration::from_secs(1));
    while watcher.changed().await.is_ok() {
        throttle.tick().await;
        let state = watcher.borrow_and_update().clone();
        let probes: Vec<String> = state
            .probes
            .iter()
            .map(|p| match p.celsius {
                Some(c) => format!("{}={c:.1}", p.name),
                None => format!("{}=?", p.name),
            })
            .collect();
        tracing::debug!(
            setpoint_c = state.setpoint_c,
            probes = probes.join(" "),
            power_fraction = state.power_fraction,
            miner_online = state.miner.online,
            miner_power_w = state.miner.power_w,
            miner_power_fraction = state.miner.power_fraction,
            "state"
        );
    }
}

async fn wait_for_shutdown() -> Result<()> {
    let mut term = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r.context("install SIGINT handler")?,
        _ = term.recv() => {}
    }
    Ok(())
}
