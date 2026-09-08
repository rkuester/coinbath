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
just checks     # fmt, clippy, tests
just run        # the daemon here, against the simulated bath
just install    # build on the Pi, install, and start the service
```

The daemon reads `/etc/coinbath.toml`, or the file named by
`COINBATH_CONFIG`. `coinbath.toml` in this directory is the
reference configuration.

## API

The daemon listens on `127.0.0.1:7786` unless `api_listen` in the
configuration says otherwise. Temperatures are degrees Celsius.

```bash
# The snapshot: setpoint, probes, miner
curl http://127.0.0.1:7786/api/v0/state

# Set the bath to 51.1 C; the reply is the snapshot after the change
curl -X PUT -H 'content-type: application/json' -d '51.1' \
  http://127.0.0.1:7786/api/v0/setpoint
```

A setpoint outside 0 to 95 C is refused with 400 and the reason.

`coinbath-cli` wraps the same two calls:

```bash
coinbath-cli status          # setpoint, probes, miner, in C and F
coinbath-cli json            # the raw snapshot
coinbath-cli setpoint 51.1   # degrees Celsius
coinbath-cli setpoint -f 124 # degrees Fahrenheit
```

## Hardware

- Raspberry Pi 5, shared with mujina-minerd
- ADS1015 ADC on I2C bus 1 at address 0x48, reading three NTC
  thermistors: bath, plate inlet, plate outlet
- A 10-inch HDMI touchscreen on the Linux framebuffer
- EmberOne hash boards with water plates, under mujina-minerd

## License

GPL-3.0-or-later.
