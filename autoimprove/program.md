# Orca Auto-Improvement Program

You are an autonomous agent improving the Orca CLI — a Rust tool that orchestrates AI coding agents via tmux and git worktrees.

## Project Context

- **Language**: Rust (edition 2024 per Cargo.toml; use latest stable toolchain, e.g. `rustup update stable`)
- **Build**: `cargo build`, `cargo test`, `cargo fmt`, `cargo clippy -- -D warnings`
- **Source**: `src/` — modules: cli, config, spawn, state, events, daemon, tmux, wake, prompts, worktree, names
- **Tests**: Unit tests inline in each module + integration tests in `tests/`
- **CI gate**: fmt + clippy + test must all pass

## What You Can Do

- Fix failing tests (read the test, understand the intent, fix the code)
- Fix clippy warnings
- Fix formatting issues
- Add missing tests for untested code paths
- Simplify overly complex code
- Fix actual bugs you discover while reading code

## What You Must NOT Do

- Add unnecessary abstractions, wrappers, or "nice to have" refactors
- Add comments to code you didn't change
- Modify test expectations just to make them pass (fix the actual code)
- Add new dependencies
- Change the public CLI interface
- Make cosmetic-only changes (renaming for style, reordering imports)
- Add feature flags or backwards-compat shims

## Strategy

1. **Fix what's broken first** — failing tests and clippy errors before anything else
2. **One fix per iteration** — smallest useful change, easy to review
3. **Read before writing** — understand the code before changing it
4. **Test your fix** — run the CI commands before finishing
5. **Commit with a clear message** — describe what you fixed and why
