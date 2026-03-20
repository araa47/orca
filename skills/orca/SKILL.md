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

## Required flags (agents)

Orca rejects ambiguous spawns so the daemon always knows **who to notify** and **parent → child** links (needed for idle detection, hook `done` deferral, and cleanup).

### You orchestrate from Claude Code, Codex, or Cursor (tmux)

- Set **`--orchestrator`** to how *you* receive notifications: **`cc`**, **`cx`**, or **`cu`** (aligned with worker backend families).
- First-level workers from your pane: **`--depth 0`** (default) is correct.
- **`--pane`** is usually auto-detected; omit it unless you know you need it.

### You orchestrate from OpenClaw

- Set **`--orchestrator openclaw`**.
- **Also required:** **`--reply-channel`** and **`--reply-to`** (and **`--reply-thread`** when replying in a Slack thread). Without these, `orca spawn` fails by default so users don’t miss completions. For automation only, the process may set `ORCA_ALLOW_OPENCLAW_WITHOUT_REPLY=1`.

### You are a worker spawning sub-workers

- Prefer running **`orca spawn` in the parent worker’s pane** so **`ORCA_WORKER_NAME`** is set; the daemon then infers **`--spawned-by`** and depth from state.
- If you spawn from a wrapper or subprocess that **does not** inherit `ORCA_WORKER_NAME`, you **must** pass **`--spawned-by <parent-worker-name>`** explicitly (name from `orca list`). Otherwise the child is stored as a root L1 worker with no parent — the same class of bug as “orchestrated finished but delegates still running.”
- Do not pass a different **`--spawned-by`** than the worker whose shell is running the command when `ORCA_WORKER_NAME` is set to a **tracked** worker; Orca will error.

### Headless / scripts (not interactive agents)

- To use **`--orchestrator none`**, set **`ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR=1`** on that process.

## CLI

```bash
orca spawn "fix the login bug" -b cc -d ~/proj --orchestrator cc
orca spawn "add unit tests" -b cx -d ~/proj --base-branch develop --orchestrator cc
orca spawn "refactor auth" -b cu -d ~/proj --orchestrator cu

# OpenClaw with reply routing (notifications go to the right channel/thread)
orca spawn "fix the login bug" -b cc -d ~/proj --orchestrator openclaw \
  --reply-channel slack --reply-to C0AGZA4178Q --reply-thread 1234567890.123456

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

The `--pane` flag is auto-detected from tmux. You almost never need to pass it explicitly.

## Backends

| Flag | Agent |
|------|-------|
| `-b cc` | Claude Code |
| `-b cx` | Codex |
| `-b cu` | Cursor Agent |

## Orchestrator Types

| Type | How you get notified |
|------|---------------------|
| `cc` / `cx` / `cu` | Message sent to your tmux pane (auto-detected) |
| `openclaw` | `openclaw system event` — pass `--reply-channel`/`--reply-to`/`--reply-thread` for routed delivery |
| `none` | Check manually with `orca list` |

## OpenClaw Orchestrator — Critical Rules

When `--orchestrator openclaw` is used, the daemon fires `openclaw system event` on completion.
This injects a heartbeat into the OpenClaw session — **but the user won't see it unless you DM them directly.**

**Always pass `--reply-channel` and `--reply-to` when the user is waiting for results:**

```bash
# Slack DM to user
orca spawn "task" -b cc -d ~/proj --orchestrator openclaw \
  --reply-channel slack --reply-to U02VA0Z3VLY

# Slack channel
orca spawn "task" -b cc -d ~/proj --orchestrator openclaw \
  --reply-channel slack --reply-to C0AGZA4178Q

# With thread (reply in thread)
orca spawn "task" -b cc -d ~/proj --orchestrator openclaw \
  --reply-channel slack --reply-to C0AGZA4178Q --reply-thread 1234567890.123456
```

**When you receive the completion event:**
1. Run `orca logs <name>` to review the output
2. Summarize the results (include PR links if any)
3. Send the summary via `message(action=send, channel=slack, target="user:U02VA0Z3VLY", message=...)`
   — **do NOT just reply in-session**, the user won't see that
4. Kill the worker: `orca kill <name>`

**Why this matters:** The system event fires as a heartbeat turn, invisible to the user in chat.
You must proactively DM/message them with the result. If you reply in-session only, they never see it.

## Sub-Workers

Workers can spawn sub-workers. Pass `--depth` and `--spawned-by`:

```bash
orca spawn "sub-task A" -b cx -d ~/proj --depth 1 --spawned-by my-worker --orchestrator cc
```

Max depth is 3 (`ORCA_MAX_DEPTH`). Max 10 running workers per orchestrator (`ORCA_MAX_WORKERS`). At max depth, do the work yourself.

### Cleanup responsibility

- **L1+ workers** (depth >= 1): Before reporting done, kill your sub-workers with `orca gc --mine`. You spawned them, you clean them up.
- **L0 orchestrator** (top-level): Do NOT auto-clean workers. The human decides when to kill/gc — they may want to inspect logs or cherry-pick branches first.

## DO

- Spawn workers for independent tasks that can run in parallel
- After spawning, stop and wait silently -- the daemon notifies you when workers finish
- Use `orca list` / `orca status` only when the user asks what's happening
- Kill individual workers when done: `orca kill <name>` (L0 only — let the human decide)
- If you're an L1+ worker, run `orca gc --mine` before reporting done to clean up your sub-workers
- Use `--orchestrator` so you get notified automatically
- Pass `--depth` and `--spawned-by` when you are a sub-worker spawning further sub-workers
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
