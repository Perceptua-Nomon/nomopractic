VERSION ?=
PI_HOST ?=

.PHONY: build test lint format check clean release deploy deploy-local coverage help

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

deploy-local:
	./scripts/deploy.sh --local $(PI_HOST)

help:
	@echo "Available targets:"
	@echo "  build        - Compile debug build"
	@echo "  test         - Run tests"
	@echo "  lint         - Check formatting and clippy warnings"
	@echo "  format       - Format code with rustfmt"
	@echo "  check        - Run lint and tests (release checks)"
	@echo "  coverage     - Generate LLVM code coverage report"
	@echo "  release      - Cross-compile release binary for aarch64"
	@echo "  deploy       - Download release from GitHub and deploy to Pi (VERSION=v0.x.y PI_HOST=user@host)"
	@echo "  deploy-local - Cross-compile and deploy current code to Pi (PI_HOST=user@host)"
	@echo "  clean        - Remove build artefacts"
	@echo "  help         - Show this help"
