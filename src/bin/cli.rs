//! Command-line client for the Coinbath API.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use coinbath::calibration::{self, Point, Points, Sample, Terms};
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

    /// Record a calibration point: average the probes for a while
    /// against a reference thermometer reading.
    Calibrate {
        /// The reference thermometer's reading, in degrees Celsius.
        #[arg(long)]
        reference: f64,

        /// A name for the point, such as "ice" or "bath-warm".
        #[arg(long)]
        label: String,

        /// Only these probes were at the reference temperature.
        /// Default: every probe.
        #[arg(long, value_delimiter = ',')]
        probes: Vec<String>,

        /// How long to average, in seconds.
        #[arg(long, default_value_t = 10)]
        seconds: u64,

        /// The points file to append to.
        #[arg(long, default_value = "calibration.toml")]
        file: PathBuf,
    },

    /// Fit coefficients from the recorded points and print them
    /// for the config, with each point's residual.
    Fit {
        /// The points file.
        #[arg(long, default_value = "calibration.toml")]
        file: PathBuf,

        /// The divider's fixed resistor, in ohms.
        #[arg(long, default_value_t = 10_000.0)]
        divider_resistor: f64,

        /// Coefficients to solve for, 2 or 3. Default: 3 with four
        /// or more points, else 2.
        #[arg(long)]
        terms: Option<u8>,
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
        Command::Calibrate {
            reference,
            label,
            probes,
            seconds,
            file,
        } => {
            let point = calibrate(&agent, &cli.url, reference, label, &probes, seconds)?;
            let mut points = read_points(&file)?;
            for s in &point.samples {
                println!(
                    "{:9}{:.4} V of {:.4} V, spread {:.4} V",
                    s.probe, s.volts, s.supply_v, s.spread_v
                );
            }
            points.points.push(point);
            std::fs::write(&file, toml::to_string(&points)?)
                .with_context(|| format!("write {}", file.display()))?;
            println!(
                "recorded point {} in {}",
                points.points.len(),
                file.display()
            );
        }
        Command::Fit {
            file,
            divider_resistor,
            terms,
        } => {
            let points = read_points(&file)?;
            let mut names: Vec<String> = points
                .points
                .iter()
                .flat_map(|p| p.samples.iter().map(|s| s.probe.clone()))
                .collect();
            names.sort();
            names.dedup();
            for name in names {
                let count = points
                    .points
                    .iter()
                    .filter(|p| p.samples.iter().any(|s| s.probe == name))
                    .count();
                let terms = match terms {
                    Some(2) => Terms::Two,
                    Some(3) => Terms::Three,
                    Some(n) => bail!("--terms must be 2 or 3, not {n}"),
                    None => Terms::for_points(count),
                };
                match calibration::fit(&points, &name, divider_resistor, terms) {
                    Ok(fit) => {
                        println!(
                            "# {name}: {count} points, {terms:?} terms; fitted minus reference"
                        );
                        for (label, reference_c, residual) in &fit.residuals {
                            println!("#   {label:12}{reference_c:7.2} C  {residual:+.3} C");
                        }
                        let t = &fit.thermistor;
                        println!("# [[probe]] name = \"{name}\"");
                        println!("a = {:.6e}\nb = {:.6e}\nc = {:.6e}\n", t.a, t.b, t.c);
                    }
                    Err(e) => println!("# {name}: {e}\n"),
                }
            }
        }
    }
    Ok(())
}

fn read_points(file: &PathBuf) -> Result<Points> {
    match std::fs::read_to_string(file) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Points::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", file.display())),
    }
}

/// Samples the daemon twice a second for `seconds` and averages
/// each chosen probe's volts and the supply.
fn calibrate(
    agent: &ureq::Agent,
    url: &str,
    reference_c: f64,
    label: String,
    probes: &[String],
    seconds: u64,
) -> Result<Point> {
    let mut volts: Vec<(String, Vec<f64>)> = Vec::new();
    let mut supply: Vec<f64> = Vec::new();
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        let state = get_state(agent, url)?;
        let Some(supply_v) = state.supply_v else {
            bail!("the daemon reports no supply voltage; set vcc_channel");
        };
        supply.push(supply_v);
        for p in &state.probes {
            if !probes.is_empty() && !probes.contains(&p.name) {
                continue;
            }
            let Some(v) = p.volts else {
                bail!(
                    "probe {} reports no volts; is the daemon on the ADC?",
                    p.name
                );
            };
            match volts.iter_mut().find(|(n, _)| *n == p.name) {
                Some((_, vs)) => vs.push(v),
                None => volts.push((p.name.clone(), vec![v])),
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if volts.is_empty() {
        bail!("no probe matched {probes:?}");
    }
    let mean = |vs: &[f64]| vs.iter().sum::<f64>() / vs.len() as f64;
    let supply_v = mean(&supply);
    let samples = volts
        .iter()
        .map(|(probe, vs)| Sample {
            probe: probe.clone(),
            volts: mean(vs),
            supply_v,
            spread_v: vs.iter().cloned().fold(f64::MIN, f64::max)
                - vs.iter().cloned().fold(f64::MAX, f64::min),
        })
        .collect();
    Ok(Point {
        label,
        reference_c,
        taken: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        samples,
    })
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
        let volts = probe
            .volts
            .map(|v| format!("  {v:.3} V"))
            .unwrap_or_default();
        match probe.celsius {
            Some(c) => println!("{:9}{}{volts}", probe.name, temp(c)),
            None => println!("{:9}no reading{volts}", probe.name),
        }
    }
    if let Some(v) = state.supply_v {
        println!("{:9}{v:.3} V", "supply");
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
