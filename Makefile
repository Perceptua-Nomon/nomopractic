.PHONY: build test check clippy fmt clean release

build:
	cargo build

test:
	cargo test

clippy:
	cargo clippy -- -D warnings

fmt:
	cargo fmt --check

check: fmt clippy test

clean:
	cargo clean

release:
	cross build --target aarch64-unknown-linux-gnu --release
