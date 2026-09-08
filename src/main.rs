use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

use coinbath::config::Config;
use coinbath::sim;
use coinbath::state::{self, Client, State};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config_path =
        std::env::var("COINBATH_CONFIG").unwrap_or_else(|_| "/etc/coinbath.toml".to_string());
    let config = Config::load(Path::new(&config_path))?;
    tracing::info!(
        setpoint_c = config.setpoint_c,
        api_listen = %config.api_listen,
        "loaded {config_path}"
    );

    let (client, state_task) = state::spawn(State::new(config.setpoint_c, &sim::PROBE_NAMES));
    let source = tokio::spawn(sim::run(client.clone()));
    let api = tokio::spawn(coinbath::api::serve(client.clone(), config.api_listen));
    let logger = tokio::spawn(log_changes(client.clone()));

    wait_for_shutdown().await?;
    tracing::info!("shutting down");

    source.abort();
    api.abort();
    logger.abort();
    drop(client);
    state_task.await.context("state task panicked")?;
    Ok(())
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
