# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.0.4] - 2026-03-20

### Added

- **Stricter `orca spawn` validation for agents:** `--orchestrator none` is rejected by default — pass `cc`, `cx`, `cu`, or `openclaw`, or set `ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR=1` for headless/scripts (e.g. autoimprove).
- **Unknown orchestrator rejection:** unknown `--orchestrator` values (typos like `ccc`, `CC`, etc.) are rejected with an error listing valid options, preventing silent notification failures.
- **`openclaw` reply routing required:** `--orchestrator openclaw` now requires `--reply-channel` and `--reply-to` unless `ORCA_ALLOW_OPENCLAW_WITHOUT_REPLY=1`.
- **Parent lineage:** `--spawned-by` must name a tracked worker; if `ORCA_WORKER_NAME` names a tracked worker it must match the resolved parent (prevents "orphan" L1 children and wrong idle / hook behavior). Stale `ORCA_WORKER_NAME` with an explicit valid `--spawned-by` is still allowed.

### Fixed

- **Flaky `state` tests:** each test using `ORCA_HOME` now gets an isolated temp dir under a mutex so parallel `cargo test` no longer races on one shared state file.

### Changed

- Agent skill (`SKILL.md`): rewritten with per-agent-type sections (Claude Code, Codex, Cursor, OpenClaw) showing exact required flags and examples. Sub-worker section clarifies `ORCA_WORKER_NAME` auto-inference vs explicit `--spawned-by`.

## [0.0.3] - 2026-03-20

### Fixed

- **Spawn depth & labels (fixes "everyone is a whale" / wrong L1):** Conceptually **L0** is the outer orchestrator (e.g. OpenClaw) and is not an Orca worker row. The **first** `orca spawn` from L0 uses `--depth 0` → stored depth **1** (🐳 L1). Delegates spawned **from inside** an existing worker pane should be **one level deeper** (🐬 L2, …), but if `--spawned-by` / depth were omitted they were all stored as depth **1**. `orca spawn` now, when `ORCA_WORKER_NAME` matches a tracked worker, infers **`--spawned-by`** and copies **parent `depth`** from state before computing the child's stored depth, so parent links and L2/L3 labels match the real tree (daemon idle logic and `has_running_children` also see correct `spawned_by`).
- **Premature "done" from lifecycle hooks while sub-workers run:** Orchestrator panes often hit the IDE **stop** hook in between turns even while they are only waiting on **child** workers. That produced `orca report --event done --source hook`, set `done_reported`, and woke the **next** orchestrator (e.g. OpenClaw) as if the L1 worker had finished. Hook-sourced **`done`** is now rewritten to **`heartbeat`** while the worker still has children in **`running`** or **`blocked`**, so completion propagation waits until delegates finish. To record a real **`done`** while children exist (rare), use `orca report --source cli`.

## [0.0.2] - 2026-03-20

_(This release was tagged without a changelog entry; summarized here retroactively.)_

### Fixed

- Tmux: add a 10s timeout around subprocess execution so the daemon cannot hang indefinitely ([#8](https://github.com/araa47/orca/pull/8)).

### Changed

- Performance: simplify Codex idle-screen detection ([#6](https://github.com/araa47/orca/pull/6)); cache `normalize_window_name` regex with `LazyLock` ([#5](https://github.com/araa47/orca/pull/5)).

### Fixed

- Truncate task previews on Unicode character boundaries to avoid UTF-8 panics ([#4](https://github.com/araa47/orca/pull/4)).

### Added

- Documentation: OpenClaw as orchestrator and agent setup for reliable Orca discovery ([#1](https://github.com/araa47/orca/pull/1), [#2](https://github.com/araa47/orca/pull/2)).

## [0.0.1] - 2025-03-19

### Added

- Initial stable Rust release.
- CLI: spawn, list, logs, steer, kill, gc, pane, report, daemon, hooks.
- Isolated workers in git worktrees; tmux-based monitoring and notifications.
- Support for OpenClaw, Claude Code, Codex, and Cursor as orchestrator backends.
- Claude Code, Codex, and Cursor as worker backends.
- Pre-commit/prek hooks (fmt, clippy, yaml, codespell); CI split into job-prek and job-test.

[Unreleased]: https://github.com/araa47/orca/compare/v0.0.4...HEAD
[0.0.4]: https://github.com/araa47/orca/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/araa47/orca/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/araa47/orca/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/araa47/orca/releases/tag/v0.0.1
