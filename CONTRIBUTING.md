# Contributing

1. Install the [Rust toolchain](https://rustup.rs/) and [tmux](https://github.com/tmux/tmux).
2. One-time test tooling (matches CI):

   ```bash
   cargo install cargo-nextest --locked
   ```

3. Clone the repo and build: `cargo build`
4. Run tests: `cargo nextest run` (fast, parallel — same runner as CI). Plain `cargo test` works if you prefer not to install nextest.
5. Make your changes.
6. Ensure formatting and lints pass: `cargo fmt --check && cargo clippy -- -D warnings`
7. Ensure all tests pass: `cargo nextest run` (or `cargo test`)
8. If you use [prek](https://github.com/j178/prek): `prek run --all-files`
9. If you bump the version in `Cargo.toml`, update [CHANGELOG.md](CHANGELOG.md) with a clear entry for the new version.
10. Submit a PR.

## Test Coverage

**Coverage must never decrease.** Every PR must maintain or improve the overall line
coverage percentage. CI automatically posts a coverage report on each pull request —
check it before requesting review.

- Every code change should include or update tests.
- Pure logic modules should maintain >95% line coverage.
- Modules touching tmux/daemon fork have lower coverage due to requiring a live tmux
  server — that's acceptable.

To check coverage locally (uses **nextest** under the hood, same as CI):

```bash
cargo install cargo-llvm-cov cargo-nextest --locked   # one-time setup
cargo llvm-cov nextest --summary-only                 # quick summary
cargo llvm-cov nextest                                # full report (stdout)
```

Current minimum: **88% line coverage**. If your PR drops below this threshold, add
tests before merging.
