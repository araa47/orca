# Agent Guidelines

- Orca is a **Rust** project. Source code is in `src/`, with `Cargo.toml` at the repo root.
- Use `cargo` for building, testing, and linting.
- Before committing: run `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo nextest run`. All must pass. (`cargo test` is an acceptable substitute if `cargo-nextest` is not installed.)

## Development

```bash
cargo install cargo-nextest --locked   # one-time; matches CI
cargo build                            # debug build
cargo nextest run                      # all tests, parallel (preferred)
cargo test                             # all tests without nextest
cargo fmt                              # format code
cargo clippy                           # lint
```

## Code Style

- Follow standard Rust idioms and naming conventions.
- All warnings must be resolved (`-D warnings` in CI).
- Use `cargo fmt` (rustfmt) for formatting.
