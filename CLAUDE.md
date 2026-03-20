# Agent Guidelines

- Orca is a **Rust** project. Source code is in `src/`, with `Cargo.toml` at the repo root.
- Use `cargo` for building, testing, and linting.
- Before committing: run `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test`. All must pass.

## Development

```bash
cargo build          # debug build
cargo test           # run all tests
cargo fmt            # format code
cargo clippy         # lint
```

## Code Style

- Follow standard Rust idioms and naming conventions.
- All warnings must be resolved (`-D warnings` in CI).
- Use `cargo fmt` (rustfmt) for formatting.
