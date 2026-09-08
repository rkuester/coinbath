use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

use coinbath::config::Config;
use coinbath::state::{self, Client, State};
use coinbath::{api, probes, sim};

/// The coinbath daemon.
#[derive(Parser)]
#[command(name = "coinbath")]
struct Args {
    /// Configuration file.
    #[arg(long, env = "COINBATH_CONFIG", default_value = "/etc/coinbath.toml")]
    config: PathBuf,

    /// Feed the state from a simulated bath instead of the ADC.
    #[arg(long)]
    sim: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let config = Config::load(&args.config)?;
    tracing::info!(
        setpoint_c = config.setpoint_c,
        api_listen = %config.api_listen,
        probes = config.probes.probes.len(),
        sim = args.sim,
        "loaded {}",
        args.config.display()
    );

    let probe_names: Vec<&str> = if args.sim {
        sim::PROBE_NAMES.to_vec()
    } else {
        config.probes.validate().context("probes")?;
        config.probes.names()
    };
    let (client, state_task) = state::spawn(State::new(config.setpoint_c, &probe_names));

    let mut source = if args.sim {
        tokio::spawn(sim::run(client.clone()))
    } else {
        tokio::spawn(probes::run(client.clone(), config.probes.clone()))
    };
    let mut api = tokio::spawn(api::serve(client.clone(), config.api_listen));
    let logger = tokio::spawn(log_changes(client.clone()));

    // The source and the API run until shutdown; either one ending
    // early is a failure the operator must see.
    let outcome = tokio::select! {
        r = wait_for_shutdown() => r,
        r = &mut source => Err(stopped_early("source", r)),
        r = &mut api => Err(stopped_early("API", r)),
    };
    tracing::info!("shutting down");

    source.abort();
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
            power_fraction = state.miner.power_fraction,
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
