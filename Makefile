.PHONY: build test check clippy fmt clean release coverage

build:
	cargo build

test:
	cargo test

clippy:
	cargo clippy -- -D warnings

fmt:
	cargo fmt --check

check: fmt clippy test

coverage:
	rustup component add llvm-tools-preview
	cargo llvm-cov --html
	cargo llvm-cov --summary-only

clean:
	cargo clean

release:
	cross build --target aarch64-unknown-linux-gnu --release
