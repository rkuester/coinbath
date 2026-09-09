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
just install          # build in release and install the service here
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

The loop is also the fail-safe. With no bath reading, with the
bath over `max_bath_c`, or with the miner not answering, it asks
for nothing, in either mode. That covers start-up: the boards get
no power until Coinbath has water temperatures in hand and the
miner has answered. The limit lives under `[limits]` in the
configuration. The miner client asks the miner for the current
request as soon as it answers, before reading anything, so a
restarted Mujina spends as little time as it can at its own
default of full power.

Chip temperatures are the miner's business. Each EmberOne caps
its own thread as its die approaches its limit and reports the
ceiling and the share it holds; Coinbath reads both and shows a
miner capped by heat as such, and its request stands for when
the chips cool.

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

## The display

The display draws two pages on the framebuffer. The main page
shows the setpoint in Fahrenheit with a button either side that
moves it one degree, the bath, inlet, and outlet temperatures
with ten minutes of sparklines, the bath's setpoint as a
reference line, the hash rate, the power, and a bar of the share
the miner holds against the share asked. The header names the
mode and any fault, and a button flips to the details page: one
row per board with its chip, board, and regulator temperatures,
rail voltages, current, power, hash rate, and the share it holds
against its ceiling; a grid of every chip's rate in GH/s, red
where the chip has reported hardware errors; the pool, its
difficulty, the shares submitted, and the miner's uptime; and
Coinbath's own raw probe and supply volts, mode, request, and
fault.

The layout is designed for the 10-inch bar panel at 1424 by 280
and scales with the frame's height. The framebuffer's size and
pixel format are read from sysfs. Taps come from the panel over
evdev, found by its name; without a panel the display still
draws, and the API still takes the setpoint.

```bash
coinbath --display /dev/fb0       # the default without --sim
coinbath --sim --display shot.png # a PNG rewritten every frame
coinbath --sim --display none --geometry 1280x400
```

## Hardware

- Raspberry Pi 5, shared with mujina-minerd
- ADS1015 ADC on I2C bus 1 at address 0x48, reading three NTC
  thermistors: bath, plate inlet, plate outlet
- A 10-inch HDMI touchscreen on the Linux framebuffer
- EmberOne hash boards with water plates, under mujina-minerd

## License

GPL-3.0-or-later.
