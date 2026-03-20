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

## Identify yourself

Before using Orca, determine which agent you are and **jump to your section**:

| You are… | Jump to | `--spawned-by` |
|----------|---------|----------------|
| OpenClaw | [OpenClaw instructions](#openclaw-l0-orchestrator) | `openclaw` |
| Claude Code | [Claude Code instructions](#claude-code-cc--claude) | `$ORCA_WORKER_NAME` (if set), or `self` (if L0) |
| Codex | [Codex instructions](#codex-cx--codex) | `$ORCA_WORKER_NAME` (if set), or `self` (if L0) |
| Cursor | [Cursor instructions](#cursor-cu--cursor) | `$ORCA_WORKER_NAME` (if set), or `self` (if L0) |

---

## `--spawned-by` rules

Every `orca spawn` must include `--spawned-by` so the daemon knows parent → child links and where to route notifications.

**OpenClaw (L0 orchestrator):**
- `--spawned-by openclaw` — OpenClaw is the only true L0 orchestrator. It lives outside tmux and receives notifications via `openclaw system event`.

**cc / cx / cu (always in tmux):**
- Check `ORCA_WORKER_NAME` in your environment.
  - If set: you are a worker — `--spawned-by "$ORCA_WORKER_NAME"` (the plain name from `orca list`, e.g. `fin`, `mud`), **NOT** the emoji label, **NOT** `openclaw`.
  - If not set: you are the L0 orchestrator — `--spawned-by self`. Orca auto-detects or generates your L0 pane name.

**Workers spawning sub-workers:**
- Same rule: `--spawned-by <your-worker-name>` from `ORCA_WORKER_NAME` or `orca list`.

---

## OpenClaw (L0 orchestrator)

You are an OpenClaw agent — the **only** true L0 orchestrator. Notifications go via `openclaw system event`, not tmux. The user only sees results if you route them explicitly via `openclaw message send`.

```bash
orca spawn "fix the login bug" -b cc -d ~/proj --orchestrator openclaw \
  --reply-channel slack --reply-to C0AGZA4178Q --reply-thread 1234567890.123456 \
  --spawned-by openclaw
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator openclaw` | **Yes** | |
| `--spawned-by openclaw` | **Yes** | L0 orchestrator marker |
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

---

## Claude Code (`cc` / `claude`)

**You must be running inside a tmux pane.** Orca auto-detects your tmux pane for notification delivery — this does not work outside tmux. If a human launched you as the orchestrator, they must have started your session inside a tmux window first.

Check `ORCA_WORKER_NAME` in your environment:
- If set: you are a worker — `--spawned-by "$ORCA_WORKER_NAME"`
- If not set: you are the L0 orchestrator — `--spawned-by self`

```bash
# As a worker (ORCA_WORKER_NAME is set):
orca spawn "fix the login bug" -b cc -d ~/proj \
  --orchestrator cc --spawned-by "$ORCA_WORKER_NAME"

# As L0 orchestrator (ORCA_WORKER_NAME is not set):
orca spawn "fix the login bug" -b cc -d ~/proj \
  --orchestrator cc --spawned-by self
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator cc` | **Yes** | Tells the daemon to send completions to your tmux pane |
| `--spawned-by <name>` | **Yes** | `$ORCA_WORKER_NAME` if set, or `self` if L0 |
| `--pane` | No | Auto-detected from your current tmux pane — omit unless overriding |
| `--depth` | No | Auto-resolved from your parent's depth |

---

## Codex (`cx` / `codex`)

**You must be running inside a tmux pane.** If a human launched you as the orchestrator, they must have started your session inside a tmux window first.

Check `ORCA_WORKER_NAME` in your environment:
- If set: you are a worker — `--spawned-by "$ORCA_WORKER_NAME"`
- If not set: you are the L0 orchestrator — `--spawned-by self`

```bash
# As a worker (ORCA_WORKER_NAME is set):
orca spawn "add unit tests" -b cx -d ~/proj \
  --orchestrator cx --spawned-by "$ORCA_WORKER_NAME"

# As L0 orchestrator (ORCA_WORKER_NAME is not set):
orca spawn "add unit tests" -b cx -d ~/proj \
  --orchestrator cx --spawned-by self
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator cx` | **Yes** | |
| `--spawned-by <name>` | **Yes** | `$ORCA_WORKER_NAME` if set, or `self` if L0 |
| `--pane` | No | Auto-detected |

---

## Cursor (`cu` / `cursor`)

**You must be running inside a tmux pane.** If a human launched you as the orchestrator, they must have started your session inside a tmux window first.

Check `ORCA_WORKER_NAME` in your environment:
- If set: you are a worker — `--spawned-by "$ORCA_WORKER_NAME"`
- If not set: you are the L0 orchestrator — `--spawned-by self`

```bash
# As a worker (ORCA_WORKER_NAME is set):
orca spawn "refactor auth" -b cu -d ~/proj \
  --orchestrator cu --spawned-by "$ORCA_WORKER_NAME"

# As L0 orchestrator (ORCA_WORKER_NAME is not set):
orca spawn "refactor auth" -b cu -d ~/proj \
  --orchestrator cu --spawned-by self
```

| Flag | Required? | Notes |
|------|-----------|-------|
| `--orchestrator cu` | **Yes** | |
| `--spawned-by <name>` | **Yes** | `$ORCA_WORKER_NAME` if set, or `self` if L0 |
| `--pane` | No | Auto-detected |

---

### Sub-workers (worker spawning further workers)

If you are a worker spawning sub-workers:

- **Always** pass `--spawned-by <your-worker-name>` — the plain name from `ORCA_WORKER_NAME` or `orca list` (e.g. `fin`, `mud`), **NOT** the emoji label.
- Only OpenClaw uses `--spawned-by openclaw`. If you are a worker, you must use your own worker name.
- Orca fails closed if you omit `--spawned-by` or pass the wrong name.

```bash
# Example: you are worker "fin" spawning sub-workers
orca spawn "sub-task A" -b cx -d ~/proj --orchestrator cc --spawned-by fin
orca spawn "sub-task B" -b cc -d ~/proj --orchestrator cc --spawned-by fin
```

Max depth is 3 (`ORCA_MAX_DEPTH`). Max 10 running workers per orchestrator (`ORCA_MAX_WORKERS`). At max depth, do the work yourself.

---

### Headless / scripts (not interactive agents)

To use `--orchestrator none`, set `ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR=1`.

---

## CLI reference

```bash
orca spawn "fix the login bug" -b cc -d ~/proj --orchestrator openclaw --spawned-by openclaw
orca spawn "add unit tests" -b cx -d ~/proj --base-branch develop --orchestrator cx --spawned-by fin
orca spawn "refactor auth" -b cu -d ~/proj --orchestrator cu --spawned-by ace

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

- **Do NOT kill or gc workers automatically.** The human decides when to kill/gc — they may want to inspect logs, review branches, or cherry-pick work first.
- If the human explicitly asks you to clean up, use `orca gc --mine` (safe, scoped to your workers only).

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
- Do NOT kill or gc workers yourself — the human decides when to clean up
- Always pass `--orchestrator` with the correct value for your agent type

## DON'T

- **NEVER run `orca kill`, `orca killall`, or `orca gc` unless the human explicitly asks** -- killing workers can destroy in-progress work and kill orchestrator panes
- Don't sleep or poll -- no `sleep`, no `orca list` loops, no periodic checks. Just stop and wait for the daemon notification.
- Don't use tmux commands directly -- always go through `orca`
- Don't spawn more than 4-5 workers at once unless explicitly asked
- Don't steer workers with huge messages -- spawn a fresh worker instead
- Don't spawn sub-workers if you're at max depth -- do the work yourself
- Don't stop the daemon (`orca daemon stop`) -- other orchestrators share it
