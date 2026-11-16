# The gate order is the CI order: formatting, lints, architecture, tests.
# Every recipe assumes only .cargo/config.toml: no exported CC, no PATH edits,
# no environment variables beyond what cargo reads itself.

.PHONY: build test fmt lint arch check bench release

build:
	cargo build --release --offline

test:
	cargo test --offline

fmt:
	cargo fmt --check

lint:
	cargo clippy -- -D warnings

arch:
	cargo run -q -p archcheck --offline --

check: fmt lint arch test

bench:
	cargo bench --offline

# Cross builds are compile checks only: x86_64-unknown-linux-gnu and
# aarch64-apple-darwin are installed, but no linker for either triple exists on
# this host. Do not turn this into a link step and call it verified.
#
# forge-cli is excluded because rustls/ring compiles C and needs a target cc;
# this host has none. The other nine crates are pure Rust and can be checked.
release:
	@echo "release: compile-checking installed cross targets only (no linkers on this host)"
	@echo "release: forge-cli excluded (rustls/ring needs a target C compiler)"
	cargo check --release --offline --target x86_64-unknown-linux-gnu --workspace --exclude forge-cli
	cargo check --release --offline --target aarch64-apple-darwin --workspace --exclude forge-cli
