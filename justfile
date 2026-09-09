_default:
    @just --list --unsorted

# Format source code
[group('dev')]
fmt *args:
    cargo fmt {{args}}

# Run clippy with warnings as errors
[group('dev')]
lint:
    cargo clippy --locked --all-targets -- -D warnings

# Run unit tests
[group('dev')]
test:
    cargo test --locked

# Run all checks (before commit, push, merge, release)
[group('dev')]
@checks: (fmt "--check") lint test

# Run the daemon here with the simulated bath
[group('dev')]
run *args:
    RUST_LOG=coinbath=debug cargo run --locked --bin coinbath -- --config coinbath.toml --sim {{args}}

# Run the CLI here
[group('dev')]
cli *args:
    cargo run -q --locked --bin coinbath-cli -- {{args}}

# Build in release, install binaries, config, and service here, restart
[group('service')]
install:
    cargo build --locked --release
    sudo install -m 755 target/release/coinbath target/release/coinbath-cli /usr/local/bin/
    test -f /etc/coinbath.toml || sudo install -m 644 coinbath.toml /etc/coinbath.toml
    sudo install -m 644 systemd/coinbath.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl enable coinbath
    sudo systemctl restart coinbath

# Stop, disable, and remove the service and the binaries
[group('service')]
uninstall:
    sudo systemctl disable --now coinbath || true
    sudo rm -f /etc/systemd/system/coinbath.service /usr/local/bin/coinbath /usr/local/bin/coinbath-cli
    sudo systemctl daemon-reload

# Restart the service
[group('service')]
restart:
    sudo systemctl restart coinbath

# Show the service status
[group('service')]
status:
    systemctl status coinbath

# Follow the service journal
[group('service')]
logs:
    journalctl -u coinbath -f
