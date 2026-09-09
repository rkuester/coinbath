# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What Coinbath is

Coinbath is a sous vide controller heated by a Bitcoin miner. It is
a companion to mujina-miner (`../mujina/`). The miner heats the
water; Coinbath reads thermistors, holds a setpoint, and asks the
miner for a share of its full power through the Mujina REST API at
`http://127.0.0.1:7785`. The API offers Coinbath
`target_power_fraction`, 0.0 to 1.0, and never a frequency.

Read `README.md` first.

## Shape

One process, one binary, one systemd unit. `state.rs` holds the
state task: it owns the snapshot, applies commands, and publishes
over a `tokio::sync::watch` channel. Every client reads the
snapshot through `Client` and changes it only by sending a
`Command`. This is the rule that keeps curl useful as the debugger
and the UI honest. Do not add a second owner of any of the state.

Temperatures are degrees Celsius everywhere inside the program.
Unit conversion belongs to whatever renders a number for a person.

## Layout

- `main.rs` starts the state task, the data source, and the
  clients, and stops them on SIGINT or SIGTERM
- `state.rs` snapshot, commands, and the state task
- `api.rs` the HTTP JSON API, a client of the state task
- `config.rs` the TOML configuration file
- `probes.rs` the thermistor reader, a source that feeds the state
  task from the ADC every 500 ms
- `adc.rs` the ADS1015 driver
- `thermistor.rs` divider and Steinhart-Hart conversion
- `control.rs` the control loop and the fail-safe, a client that
  asks for a share of full power from the bath's error every 2 s in
  automatic mode and for nothing on a fault; chip temperatures are
  the miner's business, and each board caps its own thread
- `mujina.rs` the Mujina client, a source that polls the miner's
  tree every 2 s and writes the requested power fraction to it
- `sim.rs` a simulated bath and a simulated miner, the sources
  used with `--sim`; `--sim=bath` keeps the real Mujina client
- `bin/cli.rs` the command-line client, which talks to the API
  and never to the state task directly

`../coinbath-v1/` is the previous version. Borrow from it only
where it agrees with the shape above.

## Build and run

```bash
just checks       # fmt --check, clippy -D warnings, tests
just run          # the daemon here, simulated bath, debug logs
just cli status   # the CLI against the daemon running here
just install      # build on the Pi and restart the service
just logs         # follow the service journal on the Pi
```

The Pi is `coinbath` on Tailscale. Always ask before running
anything on the Pi. The rig boards have water plates and no fans,
so they must not hash without water flowing.

## Style

Follow `.editorconfig`. Wrap markdown prose at 72 characters. Write
unit tests for behavior, not for constants. Use `anyhow` for
application errors. Comments say why, not what.
