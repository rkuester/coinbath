host := "coinbath"
remote_dir := "~/mujina/coinbath"

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
    RUST_LOG=coinbath=debug COINBATH_CONFIG=coinbath.toml cargo run --locked --bin coinbath {{args}}

# Sync coinbath source to the Pi
[group('remote')]
deploy:
    rsync -a --delete --exclude target/ --exclude .git/ . {{host}}:{{remote_dir}}/

# Deploy and run the daemon on the Pi in the foreground
[group('remote')]
remote-run *args: deploy
    ssh -t {{host}} "cd {{remote_dir}} && RUST_LOG=coinbath=debug cargo run --locked --bin coinbath {{args}}"

# Build on the Pi, install binary, config, and service, enable and restart
[group('remote')]
install: deploy
    ssh {{host}} "cd {{remote_dir}} && cargo build --locked --release"
    ssh {{host}} "sudo install -m 755 {{remote_dir}}/target/release/coinbath /usr/local/bin/"
    ssh {{host}} "test -f /etc/coinbath.toml || sudo install -m 644 {{remote_dir}}/coinbath.toml /etc/coinbath.toml"
    ssh {{host}} "sudo install -m 644 {{remote_dir}}/systemd/coinbath.service /etc/systemd/system/"
    ssh {{host}} "sudo systemctl daemon-reload && sudo systemctl enable coinbath && sudo systemctl restart coinbath"

# Stop, disable, and remove the service and binary
[group('remote')]
uninstall:
    ssh {{host}} "sudo systemctl disable --now coinbath || true"
    ssh {{host}} "sudo rm -f /etc/systemd/system/coinbath.service /usr/local/bin/coinbath"
    ssh {{host}} "sudo systemctl daemon-reload"

# Restart the coinbath service
[group('remote')]
restart:
    ssh {{host}} "sudo systemctl restart coinbath"

# Show service status
[group('remote')]
status:
    ssh {{host}} "systemctl status coinbath"

# Follow the service journal
[group('remote')]
logs:
    ssh {{host}} "journalctl -u coinbath -f"
