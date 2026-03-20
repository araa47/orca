# Orca — Architecture & Design

> A plain-English guide to how Orca works under the hood.
> For installation and usage, see the [README](README.md).

---

## Table of Contents

- [Legend](#legend)
- [Overview](#overview)
- [Core Concepts](#core-concepts)
  - [Orchestrators](#orchestrators)
  - [Workers](#workers)
  - [The Daemon](#the-daemon)
- [Lifecycle](#lifecycle)
  - [Spawning a Worker](#spawning-a-worker)
  - [While a Worker Is Running](#while-a-worker-is-running)
  - [When a Worker Finishes](#when-a-worker-finishes)
  - [Cleanup](#cleanup)
- [Use Cases](#use-cases)
- [Stuck Worker Handling](#stuck-worker-handling)
- [Isolation & Safety](#isolation--safety)
- [Notifications](#notifications)
- [Lifecycle Hooks](#lifecycle-hooks)
- [Storage Layout](#storage-layout)
- [Visual Language](#visual-language)

---

## Legend

**cc** = Claude Code · **cu** = Cursor · **cx** = Codex · **oc** = OpenClaw

## Overview

Orca is a tool that lets an AI coding agent (cc, cx, cu, or oc) **spawn multiple other AI agents to work in parallel**, each on their own task, without stepping on each other's toes. Think of it like giving your AI a team of developers — it hands out assignments, watches over them, and gets notified when they're done.

**The human doesn't use Orca directly.** The human talks to their AI agent ("build me a payment system"), and the AI agent decides to use Orca to break the work into pieces and farm them out.

---

## Core Concepts


### Orchestrators

The **orchestrator** is the AI agent that's in charge — the one the human is talking to. It reads the Orca skill, learns how to use the CLI, and then:

- Spawns workers for sub-tasks
- Gets notified when they finish
- Reviews their output
- Decides what happens next

The human tells the orchestrator what they want; the orchestrator figures out how to parallelize it.

The orchestrator is either oc or cc/cx/cu.

### Workers

A **worker** is an AI agent that's been given a task. When the orchestrator spawns a worker, Orca:

1. Creates a separate copy of the code (a **git worktree**) so the worker has its own sandbox
2. Opens a new terminal window (inside **tmux**) for the worker to run in
3. Launches the agent with the task description
4. Starts recording everything the agent does

Each worker gets a short, random name (like `fox` or `ace`) for easy reference.

Workers are cc, cx, or cu (oc workers not supported).

### The Daemon

The **daemon** is a background process that quietly watches all workers. It does three things:

1. **Detects completion** — terminal closes or agent goes idle
2. **Detects blockers** — agent is stuck on a question or prompt
3. **Sends notifications** — tells the orchestrator about these events

Nobody interacts with the daemon directly. It starts automatically and runs silently.

---

## Lifecycle

### Prerequisites

- **Git** must be available on `PATH`. Orca creates worktrees, stashes uncommitted work, and auto-initializes repos.
- If the project directory is **not a git repo**, Orca runs `git init`, stages all files, sets a fallback identity (`Orca <orca@localhost>`), and creates an initial commit (`ensure_git_repo`). This fails fast on any error.

### Spawning a Worker

When the orchestrator runs `orca spawn "fix the login bug"`:

| Step | What Happens |
|---|---|
| **Safety checks** | Verify worker count limit (default: 10) and depth limit (default: 3) aren't exceeded |
| **Git bootstrap** | Ensure the project directory is a git repo (auto-init if needed via `ensure_git_repo`) |
| **Code isolation** | Create a git worktree on a fresh branch — the worker can edit files freely without conflicts |
| **Terminal setup** | Open a new tmux window, named with the worker's name and depth emoji |
| **Agent launch** | Start the AI agent with the task description in autonomous mode |
| **Logging** | Capture all terminal output to a log file for later review |
| **State tracking** | Save worker info (name, task, status, etc.) to the state file |
| **Daemon activation** | Start the background daemon if it isn't running yet |

If anything fails, Orca cleans up after itself — removes the worktree (stashing unchanged changes to main), closes the window, and returns an error.

### While a Worker Is Running

The daemon continuously monitors every running worker:

- **Every few seconds**, it checks each worker's terminal output for signs of trouble
- **Simple prompts** (workspace trust, permission dialogs, y/n) → auto-answered, worker continues uninterrupted
- **Complex problems** (auth failures, missing credentials, ambiguous questions) → escalated to the orchestrator with context
- **Idle too long** (default: 2 minutes) → double-checked, then flagged to the orchestrator

### When a Worker Finishes

A worker can finish in several ways:

| Method | Description |
|---|---|
| **Explicit report** | The agent runs `orca report --event done` before stopping (cleanest) |
| **Terminal closes** | The daemon detects the tmux pane died |
| **Agent goes idle** | The daemon detects prolonged inactivity at a prompt |

In all cases, the orchestrator receives a notification:

```
ORCA: worker fox (claude) finished.
  orca logs fox    -- review output
  orca steer fox   -- send follow-up
  orca kill fox    -- close and free resources (double check the worker before doing so)
```

The orchestrator then decides: review logs, send a follow-up, kill, or report back to the human.

Note: if L0 launches a worker (L1) and that worker launches sub-workers (L2), the stop hook for L1 might fire while L1 waits for L2. L0 should **not** be notified since the child process is still running. Once all children finish, L0 is notified. The daemon handles this.

### Cleanup

Before removing a worker's worktree, Orca **auto-stashes** any uncommitted changes so they are not lost:

1. If `git status --porcelain` in the worktree is non-empty, Orca runs `git stash push -u -m "orca-preserving <worker> <timestamp>"`.
2. The stash attaches to the **main repo** (not the deleted worktree path).
3. A `STASH_PRESERVE` line is appended to `audit.log` for correlation with `KILL`/`GC` events.
4. A recovery hint is printed to stderr.

To **skip stashing** (e.g. in automation), pass `--no-stash` to `kill`, `killall`, or `gc`.

**Recovering stashed work** from the project root:

```bash
git stash list                           # look for "orca-preserving <worker> …"
git stash show -p stash@{n}              # inspect the diff
git stash pop                            # or: git stash apply stash@{n}
```

| Command | Behavior |
|---|---|
| `orca kill <name>` | Stashes dirty files, closes the terminal, removes the worktree, removes from state |
| `orca gc` | Bulk cleanup of all finished/dead workers (stashes dirty files first) |
| `orca killall --mine` | Kills all workers belonging to the calling orchestrator (stashes dirty files first) |

Sub-workers (L1+) are expected to clean up after themselves before reporting done. The top-level orchestrator lets the human decide when to kill workers — they might want to inspect logs or cherry-pick branches first.

---

## Use Cases

In all of these scenarios, the **human** tells their AI agent what to do, and the **AI agent** uses Orca to parallelize the work. The human doesn't run Orca commands — the AI does.

### Divide and Conquer

> *"Build me a user registration system."*

The orchestrator breaks it into independent pieces:

```bash
orca spawn "implement the user registration API" -b cc -d ~/proj --orchestrator openclaw --spawned-by openclaw
orca spawn "build the registration form UI"      -b cc -d ~/proj --orchestrator openclaw --spawned-by openclaw
orca spawn "write tests for the auth module"     -b cx -d ~/proj --orchestrator openclaw --spawned-by openclaw
```

All three run simultaneously. As each finishes, the orchestrator reviews and reports back.

### Parallel Exploration

> *"The database query on the dashboard is slow, fix it."*

The orchestrator tries multiple approaches in parallel:

```bash
orca spawn "optimize using database indexes"       -b cc -d ~/proj --orchestrator openclaw --spawned-by openclaw
orca spawn "optimize using a caching layer"        -b cc -d ~/proj --orchestrator openclaw --spawned-by openclaw
orca spawn "optimize by denormalizing the schema"  -b cx -d ~/proj --orchestrator openclaw --spawned-by openclaw
```

When all finish, it compares results and picks the best approach.

### Fresh Context

AI agents lose track over long conversations. The orchestrator keeps things clean:

1. Spawn a worker for step 1
2. When it finishes, review output, kill it
3. Spawn a fresh worker for step 2 with context about what's done

Each worker starts with a clean context window — often better than one long session.

### Iterative Refinement

A worker did 90% of the work but missed something:

```bash
orca steer fox "also add error handling for the null user edge case"
```

This sends a follow-up directly into the worker's terminal. It continues from where it left off.

### Hierarchical Delegation

A worker becomes an orchestrator for its own sub-tasks:

```
oc  (🐋 L0 — the orchestrator)
  └── worker "ace" (🐳 L1, cc) — "implement payment system"   --spawned-by openclaw
        ├── sub-worker "fig" (🐬 L2, cc) — "build Stripe integration"  --spawned-by ace
        ├── sub-worker "kai" (🐬 L2, cx) — "build payment form"        --spawned-by ace
        └── sub-worker "sol" (🐬 L2, cx) — "write payment tests"       --spawned-by ace
```

**Lineage rules (`--spawned-by`):**

| Who is spawning | `--spawned-by` value | Stored depth |
|---|---|---|
| L0 orchestrator (oc or cc/cx/cu in tmux main window) | `openclaw`/(worker name) | L1 |
| L1 worker (e.g. `ace`) spawning sub-workers | `ace` (its own worker name) | L2 |
| L2 worker (e.g. `fig`) spawning sub-sub-workers | `fig` (its own worker name) | L3 |

- L0 is the oc orchestrator — it is **not** an Orca worker, it runs outside Orca state.
- cc/cx/cu agents always run as workers inside tmux panes. They identify themselves via `ORCA_WORKER_NAME`.
- `--spawned-by` takes the plain worker name from `orca list` (e.g. `ace`, `fig`), not the emoji label.
- `--spawned-by openclaw` is **only** for the oc L0 orchestrator. Workers must use their own name.
- If `ORCA_WORKER_NAME` is set (inside a worker pane) and the agent passes the wrong `--spawned-by`, Orca rejects the spawn with a clear error.
- Correct lineage is critical: without it, `has_running_children` returns false, done-deferral breaks, and notifications route to the wrong target.

Depth is capped so it doesn't spiral out of control. When sub-workers finish, the L1 worker reviews, cleans up, and reports back up the chain.

---

## Stuck Worker Handling

### Handled Automatically

The worker continues uninterrupted — nobody notices.

- **Workspace trust dialogs** → answered yes
- **Permission prompts** → approved
- **Simple y/n confirmations** → answered
- **Rate limit pauses** → waited out

### Escalated to the Orchestrator

The orchestrator gets a message with terminal context and suggested actions.

- **Authentication failures** — token expired, can't log in
- **Missing credentials** — needs API keys or passwords
- **Ambiguous questions** — requires domain knowledge
- **Repeated failures** — hitting the same error in a loop

---

## Isolation & Safety

### Code Isolation

Each worker gets its own **git worktree** — a lightweight, independent copy of the repository on its own branch:

- Worker A and Worker B can edit the same file without conflicting
- Changes can be reviewed independently per worker
- If a worker makes a mess, killing it cleans up everything

### Scope Isolation

When multiple orchestrators use Orca on the same machine:

- `orca killall --mine` only affects the calling orchestrator's workers
- `orca gc --mine` only cleans up the calling orchestrator's finished workers
- Attempting to kill another orchestrator's worker triggers a warning
- Sub-workers inherit their parent's scope

### Resource Limits

| Limit | Default | Env Variable | Purpose |
|---|---|---|---|
| Max depth | 3 levels | `ORCA_MAX_DEPTH` | Prevents infinite spawning chains |
| Max workers | 10 per orchestrator | `ORCA_MAX_WORKERS` | Prevents resource exhaustion |
| Agent startup wait | 45 seconds | `ORCA_SPAWN_WAIT_TIMEOUT` | How long to poll the tmux pane for agent readiness after launch (float seconds; tests may set `1`) |

Workers at max depth can still do their own work — they just can't spawn further sub-workers.

---

## Notifications

| Orchestrator | Mechanism |
|---|---|
| **cc / cx** | Message typed directly into the orchestrator's tmux pane |
| **cu** | Message sent to tmux pane 3 times (cu input is less reliable) |
| **oc** | System event via `openclaw` CLI, with optional Slack routing |
| **None** | No notifications — check manually with `orca list` |

---

## Lifecycle Hooks

Orca can install lifecycle hooks into **cc** and **cx** — small scripts that run when an agent stops, calling `orca report --event done`. Most reliable way for workers to signal completion.

**cu** doesn't support hooks, so Orca appends reporting instructions directly to the task prompt.

---

## Storage Layout

Everything lives in `~/.orca/` by default (configurable via `ORCA_HOME`):

```
~/.orca/
├── state.json           # Who's running, what they're doing, their status
├── daemon.pid           # Daemon process ID
├── daemon.log           # Daemon activity log
├── audit.log            # Every action with timestamps
├── logs/
│   ├── fox.log          # Full terminal output per worker
│   └── ace.log
└── events/
    ├── fox.jsonl         # Lifecycle events (done, blocked, heartbeat)
    └── ace.jsonl
```

The state file uses **file locking** so multiple processes (CLI, daemon, multiple agents) can safely read and write simultaneously without corruption.

### Where to Look When Things Go Wrong

| Location | What Gets Recorded |
|---|---|
| `$ORCA_HOME/audit.log` | Timestamped lines: `SPAWN`, `KILL`, `KILLALL`, `GC`, `STEER`, `STASH_PRESERVE`, daemon start/stop, hooks |
| `$ORCA_HOME/events/<worker>.jsonl` | Append-only JSON: `done`, `blocked`, `heartbeat`, `process_exit` from hooks/daemon |
| `$ORCA_HOME/logs/<worker>.log` | Full captured terminal output per worker |
| `$ORCA_HOME/daemon.log` | Daemon diagnostics and monitoring events |

`audit.log` is the best single file for correlating lifecycle events (e.g. matching a `KILL` with a preceding `STASH_PRESERVE`).

---

## Visual Language

Orca uses sea creature emojis to show hierarchy depth at a glance:

| Emoji | Level | Who | `--spawned-by` |
|---|---|---|---|
| 🐋 | L0 | Orchestrator (oc or cc/cx/cu) | — |
| 🐳 | L1 | Direct worker spawned by L0 | `openclaw` or worker name |
| 🐬 | L2 | Sub-worker spawned by L1 | parent worker name (e.g. `ace`) |
| 🐟 | L3 | Sub-sub-worker spawned by L2 | parent worker name (e.g. `fig`) |
| 🦐 | L4+ | Deepest level | parent worker name |

```
|-🐋 | L0 openclaw/ rook..
├── [orc] ace  claude  ▶ running  🐳 L1  build feature X...    2m ago
│   ├── [wrk] fig  claude  ▶ running  🐬 L2  implement auth...  1m ago
│   └── [wrk] kai  codex   ✓ done     🐬 L2  add unit tests...  3m ago
└── [wrk] sol  claude  ✓ done     🐳 L1  fix login bug...      5m ago
```

| Symbol | Meaning |
|---|---|
| `[orc]` | Has sub-workers (acting as orchestrator) |
| `[wrk]` | Leaf worker (no children) |
| `▶` | Running |
| `✓` | Done |
| `✗` | Dead (crashed) |
| `💀` | Destroyed (killed outside Orca) |

---

## Philosophy

**Orca is just plumbing.** It doesn't tell you how to organize your work. It handles the mechanics — spawning, isolation, monitoring, notifications — and leaves the *strategy* to **skills**.

Skills are reusable prompt templates and workflow definitions. The repo includes a [`sprint-team`](skills/sprint-team/SKILL.md) skill that defines roles like Researcher, Architect, Coder, and Validator. But you can write your own, use a simple "split into N chunks" approach, or have the orchestrator figure it out on the fly.

Orca doesn't care about strategy. As long as the orchestrator calls `spawn`, `steer`, and `kill`, the plumbing works the same way.
