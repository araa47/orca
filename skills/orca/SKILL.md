---
name: orca
description: >-
  Spawn and manage parallel AI coding agents via tmux. Use when you need to
  orchestrate workers, delegate sub-tasks, run multi-agent improvement loops,
  or manage agent lifecycles with orca CLI commands like spawn, list, kill,
  steer, logs, and daemon.
---
# Orca — Agent Orchestrator

One-time setup: see [references/SETUP.md](references/SETUP.md) if orca is not already on your PATH.

You are the orchestrator. Use the `orca` CLI below. You never need tmux knowledge.

## Required flags by orchestrator type

Orca **rejects** ambiguous spawns so the daemon always knows **who to notify** and **parent → child** links. Every `orca spawn` must declare an orchestrator **and** `--spawned-by`.

Valid `--orchestrator` values: **`cc`**, **`cx`**, **`cu`**, **`openclaw`** (or long forms `claude`, `codex`, `cursor`). Unknown values are rejected. `none` requires opt-in via env var.

---

### Claude Code (`cc` / `claude`)

You are running inside a Claude Code tmux pane.

```bash
orca spawn "fix the login bug" -b cc -d ~/proj --orchestrator cc --spawned-by root
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator cc` | **Yes** | Tells the daemon to send completions to your tmux pane |
| `--spawned-by root` | **Yes** | Top-level orchestrator spawn marker (`root:<scope>` also allowed) |
| `--pane` | No | Auto-detected from your current tmux pane — omit unless overriding |
| `--depth` | No | Default `0` for first-level workers — correct for L0 orchestrators |

### Codex (`cx` / `codex`)

You are running inside a Codex tmux pane.

```bash
orca spawn "add unit tests" -b cx -d ~/proj --orchestrator cx --spawned-by root
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator cx` | **Yes** | |
| `--spawned-by root` | **Yes** | Top-level orchestrator spawn marker |
| `--pane` | No | Auto-detected |

### Cursor (`cu` / `cursor`)

You are running inside a Cursor Agent tmux pane.

```bash
orca spawn "refactor auth" -b cu -d ~/proj --orchestrator cu --spawned-by root
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator cu` | **Yes** | |
| `--spawned-by root` | **Yes** | Top-level orchestrator spawn marker |
| `--pane` | No | Auto-detected |

---

### OpenClaw (`openclaw`)

You are an OpenClaw agent. Notifications go via `openclaw system event`, and the user only sees results if you route them explicitly.

```bash
orca spawn "fix the login bug" -b cc -d ~/proj --orchestrator openclaw \
  --reply-channel slack --reply-to C0AGZA4178Q --reply-thread 1234567890.123456 \
  --spawned-by root
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator openclaw` | **Yes** | |
| `--spawned-by root` | **Yes** | Top-level orchestrator spawn marker |
| `--reply-channel` | **Yes** | `slack`, `telegram`, `discord`, etc. |
| `--reply-to` | **Yes** | Channel ID or user ID for delivery |
| `--reply-thread` | No | Thread ID for threaded replies (Slack) |
| `--session-id` | No | OpenClaw session ID for scoped killall/gc |
| `--pane` | No | Not used — OpenClaw delivers via system events, not tmux panes |

**Without `--reply-channel` and `--reply-to`, `orca spawn` will fail.** Set `ORCA_ALLOW_OPENCLAW_WITHOUT_REPLY=1` only for automation.

**When you receive the completion event:**
1. Run `orca logs <name>` to review the output
2. Summarize the results (include PR links if any)
3. Send the summary via `openclaw message send --channel <ch> --target <target> --message <summary>`
4. Do NOT reply in-session — the user won't see that. Use `openclaw message send`.
5. Kill the worker: `orca kill <name>`

---

### Sub-workers (worker spawning further workers)

If you are a worker spawning sub-workers:

- **Always** pass `--spawned-by <your-worker-name>` explicitly. Orca now requires lineage on every spawn and fails closed if you omit it.
- Do not pass a different `--spawned-by` than your own name when `ORCA_WORKER_NAME` is set to a tracked worker; Orca will error.

```bash
orca spawn "sub-task A" -b cx -d ~/proj --orchestrator cc --spawned-by my-worker
```

Max depth is 3 (`ORCA_MAX_DEPTH`). Max 10 running workers per orchestrator (`ORCA_MAX_WORKERS`). At max depth, do the work yourself.

---

### Headless / scripts (not interactive agents)

To use `--orchestrator none`, set `ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR=1`.

---

## CLI reference

```bash
orca spawn "fix the login bug" -b cc -d ~/proj --orchestrator cc --spawned-by root
orca spawn "add unit tests" -b cx -d ~/proj --base-branch develop --orchestrator cx --spawned-by root
orca spawn "refactor auth" -b cu -d ~/proj --orchestrator cu --spawned-by root

orca list                                   # List all workers
orca status <name>                          # Detailed status (last output lines)
orca logs <name>                            # Full terminal output
orca steer <name> "also add tests"          # Send follow-up to a running worker
orca kill <name>                            # Kill a single worker (warns if not yours)
orca killall --mine                         # Kill YOUR workers only (safe, auto-detects pane)
orca killall --force                        # Kill ALL workers globally (requires human approval!)
orca gc --mine                              # Clean up YOUR done/dead workers
orca gc --force                             # Clean up ALL done/dead workers (requires human approval!)
orca daemon start|stop|status               # Daemon management (auto-starts on first spawn)
orca hooks install|uninstall                # Install/remove lifecycle hooks for Claude Code & Codex
orca report -w <name> -e done              # Report worker lifecycle event (used by hooks)
```

## Backends

| Flag | Agent |
|------|-------|
| `-b cc` | Claude Code |
| `-b cx` | Codex |
| `-b cu` | Cursor Agent |

## Cleanup responsibility

- **L1+ workers** (depth >= 1): Before reporting done, kill your sub-workers with `orca gc --mine`. You spawned them, you clean them up.
- **L0 orchestrator** (top-level): Do NOT auto-clean workers. The human decides when to kill/gc — they may want to inspect logs or cherry-pick branches first.

## Recovering work after `orca kill` / `gc`

When a worker is killed or garbage-collected, Orca **auto-stashes uncommitted changes** before removing the worktree. Stashes attach to the **main repo**, not the deleted worktree path.

From the **project root** (`-d` directory):

```bash
git stash list                           # look for "orca-preserving <worker> …"
git stash show -p stash@{n}              # inspect the diff
git stash pop                            # or: git stash apply stash@{n}
```

- **Committed** work on a branch is unaffected by stash; detached commits still need branches per normal Git rules.
- Pass `--no-stash` to `kill`, `killall`, or `gc` to skip stashing (automation escape hatch).
- **Debugging:** `$ORCA_HOME/audit.log` has `KILL`, `GC`, and `STASH_PRESERVE` entries; `events/<worker>.jsonl` has lifecycle events; `logs/<worker>.log` has terminal output; `daemon.log` has daemon diagnostics.

## DO

- Spawn workers for independent tasks that can run in parallel
- After spawning, stop and wait silently -- the daemon notifies you when workers finish
- Use `orca list` / `orca status` only when the user asks what's happening
- Kill individual workers when done: `orca kill <name>` (L0 only — let the human decide)
- If you're an L1+ worker, run `orca gc --mine` before reporting done to clean up your sub-workers
- Always pass `--orchestrator` with the correct value for your agent type
- Use `orca killall --mine` and `orca gc --mine` to clean up -- this only touches YOUR workers

## DON'T

- **NEVER use `orca killall --force` or `orca gc --force` unless the human explicitly asks** -- these are global and will kill other orchestrators' workers
- **NEVER run `orca kill` on a worker you didn't spawn** unless the human tells you to -- it will warn you if you try
- Don't sleep or poll -- no `sleep`, no `orca list` loops, no periodic checks. Just stop and wait for the daemon notification.
- Don't use tmux commands directly -- always go through `orca`
- Don't spawn more than 4-5 workers at once unless explicitly asked
- Don't steer workers with huge messages -- spawn a fresh worker instead
- Don't spawn sub-workers if you're at max depth -- do the work yourself
- Don't stop the daemon (`orca daemon stop`) -- other orchestrators share it
