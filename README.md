# Coinbath

Coinbath holds a sous vide bath at temperature with the waste heat
of a Bitcoin miner. The miner is the heating element. Coinbath
reads the water temperature, compares it to the setpoint, and asks
the miner for a share of its full power through the Mujina REST
API.

Coinbath is one process. A state task owns the setpoint, the probe
readings, and the miner telemetry, and publishes a snapshot over a
watch channel. The touchscreen display, the HTTP JSON API, and the
command line are clients of that snapshot. Every change, from a
touch or from an HTTP request, goes through the state task's
command channel.

## Build and run

```bash
just checks           # fmt, clippy, tests
just run              # the daemon here, simulated bath and miner
just run --sim=bath   # simulated bath, the Mujina on this machine
just install          # build on the Pi, install, and start the service
```

The daemon reads `/etc/coinbath.toml`, or the file named by
`--config` or `COINBATH_CONFIG`. `coinbath.toml` in this directory
is the reference configuration. It names the ADC bus, the divider
supply, and one `[[probe]]` per thermistor with its channel,
divider resistor, Steinhart-Hart coefficients, and calibration
offset. `mujina_url` names the miner's API, loopback port 7785
unless set. With `--sim` the daemon ignores the probes and the
miner and runs a simulated bath heated by a simulated miner. With
`--sim=bath` the simulated bath is heated by whatever share of
full power the real Mujina reports, which is how the miner client
is exercised against Mujina's CPU backend on a laptop.

## The control loop

The loop steps every 2 s in automatic mode, the mode at start. It
takes the bath probe's error from the setpoint through a
proportional-integral law with anti-windup and asks the miner for
the result, 0 to 1. The gains live under `[control]` in the
configuration; the defaults give full power from two degrees
below the setpoint and an integral of about ten minutes. Setting
the power fraction by hand puts the state in manual mode, where
the loop stays out of it. Automatic mode starts the loop afresh.

## The miner

The Mujina client polls the miner's tree at `GET /api/v0` every
2 s and reports the hash rate summed over threads, the power
summed over regulators, the hottest chip, and the share of full
power the miner holds. Coinbath asks for a share through the
state task; the client writes it to
`PUT /api/v0/target_power_fraction` when it changes, and writes it
again when the miner comes back from an outage, since a restarted
Mujina forgets. Until something asks, the miner decides for
itself, which for Mujina is full power. A miner that stops
answering is reported offline with no readings.

## API

The daemon listens on `127.0.0.1:7786` unless `api_listen` in the
configuration says otherwise. Temperatures are degrees Celsius.

```bash
# The snapshot: setpoint, probes, miner
curl http://127.0.0.1:7786/api/v0/state

# Set the bath to 51.1 C; the reply is the snapshot after the change
curl -X PUT -H 'content-type: application/json' -d '51.1' \
  http://127.0.0.1:7786/api/v0/setpoint

# Ask the miner for a quarter of its full power, by hand
curl -X PUT -H 'content-type: application/json' -d '0.25' \
  http://127.0.0.1:7786/api/v0/power_fraction

# Hand the power request back to the control loop
curl -X PUT -H 'content-type: application/json' -d '"auto"' \
  http://127.0.0.1:7786/api/v0/mode
```

A setpoint outside 0 to 95 C, or a power fraction outside 0 to 1,
is refused with 400 and the reason.

`coinbath-cli` wraps the same calls:

```bash
coinbath-cli status          # setpoint, probes, miner, in C and F
coinbath-cli json            # the raw snapshot
coinbath-cli setpoint 51.1   # degrees Celsius
coinbath-cli setpoint -f 124 # degrees Fahrenheit
coinbath-cli power 0.25      # a share of full power, by hand
coinbath-cli mode auto       # back to the control loop
```

## Hardware

- Raspberry Pi 5, shared with mujina-minerd
- ADS1015 ADC on I2C bus 1 at address 0x48, reading three NTC
  thermistors: bath, plate inlet, plate outlet
- A 10-inch HDMI touchscreen on the Linux framebuffer
- EmberOne hash boards with water plates, under mujina-minerd

## License

GPL-3.0-or-later.
