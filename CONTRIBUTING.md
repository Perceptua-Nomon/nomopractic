# Contributing to nomopractic

## Prerequisites

- Rust stable toolchain (`rustup default stable`)
- For cross-compilation: `cargo install cross`
- No Raspberry Pi required for development — all tests mock hardware

## Development Setup

```bash
git clone https://github.com/Perceptua/nomopractic.git
cd nomopractic
cargo build
cargo test
```

## Code Quality

Run all checks before committing:

```bash
cargo fmt --check       # formatting
cargo clippy -- -D warnings  # lint (warnings = errors)
cargo test              # unit + integration tests
```

Or use the Makefile:

```bash
make check   # fmt + clippy + test
```

## Code Style

- **Formatter**: `rustfmt` (default settings)
- **Linter**: `clippy` with `-D warnings`
- **Error handling**: `thiserror` for custom error types, `?` propagation
- **Logging**: `tracing` crate with structured fields
- **Naming**: snake_case functions, PascalCase types, SCREAMING_SNAKE constants
- **Safety**: No `unsafe` unless required; document with `// SAFETY:` comment
- **Tests**: `#[cfg(test)] mod tests` in each module; mock all hardware

## Module Guidelines

### Adding an IPC Method

1. Add the method name to the match in `src/ipc/handler.rs`
2. Implement the HAT driver function in the appropriate `src/hat/` module
3. Update `src/ipc/schema.rs` if new param/result types are needed
4. Add unit tests for the driver function
5. Add an IPC integration test in `tests/`
6. Update `docs/architecture.md` methods table

### Adding a HAT Driver Module

1. Create `src/hat/<module>.rs`
2. Add `pub mod <module>;` to `src/hat/mod.rs`
3. Use the I2C helpers from `src/hat/i2c.rs` — do not access rppal directly
4. Define a trait for the hardware interface (enables mocking in tests)
5. Add unit tests with mock I2C

## Testing Strategy

- **Unit tests**: In-module `#[cfg(test)]` blocks. Mock I2C via trait objects.
- **Integration tests**: `tests/` directory. Spawn daemon + connect via socket.
- **No hardware needed**: All tests pass on x86_64 Linux/macOS/Windows.
- **CI**: GitHub Actions runs `cargo test` on every push.

## Commit Style

- Short, imperative, descriptive
- Examples:
  - `Add I2C read/write helpers for HAT registers`
  - `Implement servo TTL lease watchdog`
  - `Fix ADC channel validation range`

## Cross-Compilation

Build for Raspberry Pi (aarch64):

```bash
cross build --target aarch64-unknown-linux-gnu --release
```

The binary will be at `target/aarch64-unknown-linux-gnu/release/nomopractic`.
