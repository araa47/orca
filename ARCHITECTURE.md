# Architecture

Orca is a single Rust binary (`orca`) that orchestrates AI coding agents via tmux. Agents run in isolated git worktrees, each in its own tmux window. A background daemon watches for completion and stuck prompts, then notifies the orchestrator.

## Project Layout

```
├── Cargo.toml           # Package manifest (binary: orca)
├── Cargo.lock
├── rustfmt.toml
├── src/
│   ├── main.rs          # Entry point → cli::run()
│   ├── cli.rs           # Clap command dispatch, audit logging
│   ├── config.rs        # Paths, env vars, backend binary lookup
│   ├── spawn.rs         # Full spawn flow: worktree → tmux → agent → state
│   ├── state.rs         # Worker struct, JSON persistence with flock
│   ├── events.rs        # Append-only JSONL event store per worker
│   ├── daemon.rs        # Background watchdog (tmux control-mode + poll)
│   ├── tmux.rs          # Async tmux subprocess helpers
│   ├── wake.rs          # Orchestrator notification (pane send_keys, OpenClaw)
│   ├── prompts.rs       # Stuck-prompt detection and auto-handling
│   ├── worktree.rs      # Git worktree create/remove, prek hook patching
│   └── names.rs         # Random 3-letter worker name generation
├── hooks/
│   ├── install.sh       # Install orca lifecycle hooks for Claude Code / Codex
│   ├── uninstall.sh     # Remove orca lifecycle hooks
│   └── orca-hook.sh     # Shared hook handler (called by agent stop hooks)
├── skills/
│   ├── orca/            # Orca skill (SKILL.md + SETUP.md) — agents read this
│   └── sprint-team/     # Sprint team role definitions for improvement loops
└── .github/workflows/   # CI: cargo fmt, clippy, test
```

## Module Dependency Graph

```
┌─────────────────────────────────────────────────────────────────┐
│                         CLI (cli.rs)                            │
│  spawn · list · status · logs · kill · killall · gc · steer     │
│  report · pane · daemon start/stop/status · hooks install       │
└──────────┬──────────┬──────────┬──────────┬──────────┬──────────┘
           │          │          │          │          │
     ┌─────▼──┐  ┌────▼───┐  ┌──▼──┐  ┌───▼───┐  ┌──▼────┐
     │ spawn  │  │ state  │  │tmux │  │events │  │config │
     │  .rs   │  │  .rs   │  │ .rs │  │  .rs  │  │  .rs  │
     └───┬────┘  └────────┘  └──┬──┘  └───────┘  └───────┘
         │                      │
    ┌────▼─────┐          ┌─────▼─────┐
    │worktree  │          │  daemon   │
    │   .rs    │          │   .rs     │
    └──────────┘          └─────┬─────┘
                                │
                    ┌───────────┼───────────┐
                    │           │           │
               ┌────▼──┐  ┌────▼──┐  ┌─────▼────┐
               │ wake  │  │prompts│  │  tmux    │
               │  .rs  │  │  .rs  │  │ control  │
               └───────┘  └───────┘  │  mode    │
                                     └──────────┘
```

## Module Reference

| Module | Purpose |
|---|---|
| **cli** | Clap-based command dispatch, audit logging to `audit.log`, tree printing, ANSI stripping |
| **config** | Paths (`$ORCA_HOME`), env var overrides, backend binary lookup (`cc`→`claude`, `cx`→`codex`), tmux session detection |
| **state** | `Worker` struct with serde, JSON persistence under `state.json` with `flock`-based locking, save/load/update/gc |
| **events** | Append-only JSONL event store under `events/{name}.jsonl` — event types: `done`, `blocked`, `heartbeat`, `process_exit` |
| **spawn** | Full spawn flow: validate args → ensure git repo → create worktree → create tmux window → launch agent with env vars → save worker |
| **daemon** | Async background watchdog: tmux control-mode attachment for real-time pane death events, plus poll loop for idle/stuck detection. Single-instance via PID file + flock. SIGUSR1 nudge support. |
| **tmux** | Low-level tmux subprocess wrappers: session/window/pane CRUD, capture-pane, send-keys, pipe-pane, control-mode read |
| **wake** | Orchestrator notification: sends completion/stuck messages to the orchestrator's tmux pane, or fires `openclaw system event` for OpenClaw orchestrators |
| **prompts** | Pattern matching for stuck prompts (workspace trust, y/n confirmations, rate limits) and auto-handling via send-keys |
| **worktree** | Git operations: repo init, identity setup, worktree create/remove, prek hook patching in worktrees |
| **names** | Generates random 3-letter worker names from a curated wordlist with collision avoidance |

## Data Flow

### Spawn Flow

1. CLI validates args, checks depth limit (`ORCA_MAX_DEPTH`) and worker count (`ORCA_MAX_WORKERS`)
2. `spawn.rs` ensures the project has a git repo, creates a git worktree under `.worktrees/`
3. A tmux window is created in the orca session with `ORCA_WORKER_*` env vars
4. The agent binary is launched with the task prompt (written to a temp file for safe shell escaping)
5. `tmux pipe-pane` captures terminal output to `logs/{name}.log`
6. Worker state is saved to `state.json`; daemon is auto-started if not running

### Event-Driven Lifecycle

1. Agent calls `orca report --worker <name> --event done` (via injected instructions or lifecycle hooks)
2. Event is appended to `events/{name}.jsonl`; worker fields updated atomically under flock
3. Daemon picks up `done_reported=true` → marks worker done → wakes the orchestrator
4. Fallback: if no report events, the watchdog scans the pane for idle/stuck patterns after `WATCHDOG_QUIET_SECS`

### Stuck-Prompt Handling

1. Daemon captures the last N lines of each running worker's pane
2. `prompts.rs` matches known patterns: workspace trust dialogs, permission prompts, y/n confirmations, rate-limit messages
3. Simple prompts are auto-answered (e.g., sending "y" or "Enter")
4. Complex blockers (auth failures, ambiguous questions) are escalated to the orchestrator with context

### Orchestrator Isolation

- Each orchestrator's workers are scoped by `orchestrator_pane` or `session_id`
- `killall --mine` / `gc --mine` only affect the calling orchestrator's workers
- `kill` warns if targeting another orchestrator's worker
- Children (workers spawned by workers) inherit their parent's scope

## Runtime State

All state lives under `$ORCA_HOME` (default: `~/.orca/`):

| Path | Format | Purpose |
|---|---|---|
| `state.json` | JSON | All workers: name, status, pane, task, orchestrator, depth, timestamps |
| `daemon.pid` | Text | Daemon PID (flock held for single-instance guarantee) |
| `daemon.log` | Text | Daemon lifecycle events: pane death, idle detection, wakes, errors |
| `audit.log` | Text | CLI audit trail: every spawn, kill, gc, steer, report with timestamps |
| `events/{name}.jsonl` | JSONL | Per-worker event log: done, blocked, heartbeat, process\_exit |
| `logs/{name}.log` | Text | Per-worker terminal output captured via tmux pipe-pane |

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `ORCA_HOME` | `~/.orca` | State and log directory |
| `ORCA_TMUX_SESSION` | auto-detect | Tmux session name for orca windows |
| `ORCA_MAX_DEPTH` | `3` | Max orchestration depth (how many levels of sub-workers) |
| `ORCA_MAX_WORKERS` | `10` | Max running workers per orchestrator |
| `ORCA_WATCHDOG_QUIET_SECS` | `120` | Seconds of inactivity before watchdog scanning activates |

## Lifecycle Hooks

Orca installs hooks into Claude Code and Codex so that when an agent stops, it automatically calls `orca report --event done`. The hooks are shell scripts embedded in the binary via `include_str!` and installed/uninstalled via `orca hooks install` / `orca hooks uninstall`.

- **Claude Code**: Hooks into `~/.claude/settings.json` → `hooks.Stop` (runs on task completion)
- **Codex**: Hooks into `~/.codex/config.yaml` → `notify` (runs on agent exit)
- **Cursor**: No native hook support — orca injects reporting instructions into the spawn prompt instead

## CI

GitHub Actions runs on every PR and push to main:

- `cargo fmt --check` — formatting
- `cargo clippy -D warnings` — linting
- `cargo test` — all tests (529 as of this writing: 475 unit + 54 integration)

Test coverage is **88% line coverage**. Pure logic modules (prompts, events, names, state, tmux, wake) maintain >95%. Modules that interact with tmux/daemon fork have lower coverage due to requiring a live tmux server.
