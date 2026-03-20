# Contributing

1. Install the [Rust toolchain](https://rustup.rs/) and [tmux](https://github.com/tmux/tmux).
2. Clone the repo and build: `cargo build`
3. Run tests: `cargo test`
4. Make your changes.
5. Ensure formatting and lints pass: `cargo fmt --check && cargo clippy -- -D warnings`
6. Ensure all tests pass: `cargo test`
7. If you use [prek](https://github.com/EricCroworktree/prek): `prek run --all-files`
8. If you bump the version in `Cargo.toml`, update [CHANGELOG.md](CHANGELOG.md) with a clear entry for the new version.
9. Submit a PR.

## Test Coverage

**Coverage must never decrease.** Every PR must maintain or improve the overall line
coverage percentage. CI automatically posts a coverage report on each pull request —
check it before requesting review.

- Every code change should include or update tests.
- Pure logic modules should maintain >95% line coverage.
- Modules touching tmux/daemon fork have lower coverage due to requiring a live tmux
  server — that's acceptable.

To check coverage locally:

```bash
cargo install cargo-llvm-cov     # one-time setup
cargo llvm-cov                   # full report
cargo llvm-cov --summary-only    # summary only
```

Current minimum: **88% line coverage**. If your PR drops below this threshold, add
tests before merging.
