# Contributing

## Prerequisites

- [Git](https://git-scm.com/) 2.20+
- [tmux](https://github.com/tmux/tmux) 3.0+
- [direnv](https://direnv.net/) (recommended) — auto-sets up toolchain on `cd`

## Setup

```bash
git clone https://github.com/araa47/orca.git && cd orca
direnv allow   # installs rustup, rustfmt, clippy, cargo-nextest automatically
```

If you don't use direnv, install manually:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install cargo-nextest --locked
```

The `rust-toolchain.toml` file ensures rustup installs the correct toolchain with `rustfmt` and `clippy` components on first `cargo` invocation.

## Development

```bash
cargo build              # debug build
cargo nextest run        # run tests in parallel (preferred)
cargo test               # run tests without nextest
cargo fmt                # format code
cargo clippy             # lint
```

## Before Committing

All three must pass (pre-commit hooks enforce the first two):

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo nextest run
```

## Test Coverage

Coverage is measured in CI via `cargo-llvm-cov`. Coverage must not decrease on PRs — the CI bot posts a coverage report on every pull request.
