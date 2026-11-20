# The gate order is the CI order: formatting, lints, architecture, tests.
# Every recipe assumes only .cargo/config.toml: no exported CC, no PATH edits,
# no environment variables beyond what cargo reads itself.

.PHONY: web build test fmt lint arch check bench bench-run bench-record bench-check release

# npm uses POSIX ';' here; the managed shell runs this recipe through bash/Git-Bash.
web:
	cd web; npm ci; npm run build

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

check: fmt lint arch test bench-check

bench:
	cargo bench --offline

bench-run: build
	cargo run --release -q -p hearth-bench --offline --

bench-record: build
	cargo run --release -q -p hearth-bench --offline -- -out scripts/bench/baseline.json -baseline ""

bench-check: build
	cargo run --release -q -p hearth-bench --offline -- -strict

# Cross builds are compile checks only: x86_64-unknown-linux-gnu and
# aarch64-apple-darwin are installed, but no linker for either triple exists on
# this host. Do not turn this into a link step and call it verified.
#
# hearth-cli is excluded because rustls/ring compiles C and needs a target cc;
# this host has none. The other nine crates are pure Rust and can be checked.
release:
	@echo "release: compile-checking installed cross targets only (no linkers on this host)"
	@echo "release: hearth-cli excluded (rustls/ring needs a target C compiler)"
	cargo check --release --offline --target x86_64-unknown-linux-gnu --workspace --exclude hearth-cli
	cargo check --release --offline --target aarch64-apple-darwin --workspace --exclude hearth-cli
