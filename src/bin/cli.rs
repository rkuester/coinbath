//! Command-line client for the Coinbath API.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use coinbath::state::State;

#[derive(Parser)]
#[command(name = "coinbath-cli", about = "Talk to a running coinbath")]
struct Cli {
    /// Base URL of the coinbath API.
    #[arg(long, default_value = "http://127.0.0.1:7786")]
    url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the setpoint, the probes, and the miner.
    Status,

    /// Print the raw JSON snapshot.
    Json,

    /// Set the target bath temperature.
    Setpoint {
        /// Temperature, in degrees Celsius unless -f is given.
        temperature: f64,

        /// Read the temperature as degrees Fahrenheit.
        #[arg(short, long)]
        fahrenheit: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    match cli.command {
        Command::Status => {
            let state = get_state(&agent, &cli.url)?;
            print_status(&state);
        }
        Command::Json => {
            let state = get_state(&agent, &cli.url)?;
            println!("{}", serde_json::to_string_pretty(&state)?);
        }
        Command::Setpoint {
            temperature,
            fahrenheit,
        } => {
            let celsius = if fahrenheit {
                (temperature - 32.0) * 5.0 / 9.0
            } else {
                temperature
            };
            let mut response = agent
                .put(format!("{}/api/v0/setpoint", cli.url))
                .send_json(celsius)
                .context("send setpoint")?;
            let status = response.status();
            let body = response.body_mut().read_to_string()?;
            if !status.is_success() {
                bail!("{status}: {body}");
            }
            let state: State = serde_json::from_str(&body)?;
            println!("setpoint {}", temp(state.setpoint_c));
        }
    }
    Ok(())
}

fn get_state(agent: &ureq::Agent, url: &str) -> Result<State> {
    agent
        .get(format!("{url}/api/v0/state"))
        .call()
        .context("get state")?
        .body_mut()
        .read_json()
        .context("parse state")
}

fn print_status(state: &State) {
    println!("{:9}{}", "setpoint", temp(state.setpoint_c));
    for probe in &state.probes {
        match probe.celsius {
            Some(c) => println!("{:9}{}", probe.name, temp(c)),
            None => println!("{:9}no reading", probe.name),
        }
    }
    let miner = &state.miner;
    let hashrate = miner
        .hashrate_hs
        .map(|h| format!("{:.2} TH/s", h / 1e12))
        .unwrap_or_else(|| "unknown".into());
    let power = miner
        .power_w
        .map(|w| format!("{w:.0} W"))
        .unwrap_or_else(|| "unknown".into());
    let fraction = miner
        .power_fraction
        .map(|f| format!("{f:.2}"))
        .unwrap_or_else(|| "unset".into());
    println!(
        "{:9}{hashrate}, {power}, power fraction {fraction}",
        "miner"
    );
}

/// Formats a temperature in both units, since the bath is set in
/// Fahrenheit at the table and held in Celsius inside.
fn temp(celsius: f64) -> String {
    format!("{celsius:.1} C ({:.1} F)", celsius * 9.0 / 5.0 + 32.0)
}
