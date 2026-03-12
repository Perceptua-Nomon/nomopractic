VERSION ?=
PI_HOST ?=

.PHONY: build test lint format check clean release deploy coverage

build:
	cargo build

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy -- -D warnings

format:
	cargo fmt

check: lint test

coverage:
	rustup component add llvm-tools-preview
	cargo llvm-cov --html
	cargo llvm-cov --summary-only

clean:
	cargo clean

release:
	cross build --target aarch64-unknown-linux-gnu --release

deploy:
	./scripts/deploy.sh $(VERSION) $(PI_HOST)
