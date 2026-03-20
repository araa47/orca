# Orca — Architecture & Design

> A plain-English guide to how Orca works under the hood.
> For installation and usage, see the [README](README.md).

---

## Table of Contents

- [Overview](#overview)
- [Core Concepts](#core-concepts)
  - [Workers](#workers)
  - [Orchestrators](#orchestrators)
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

## Overview

Orca is a tool that lets an AI coding agent (Claude Code, Codex, Cursor, or OpenClaw) **spawn multiple other AI agents to work in parallel**, each on their own task, without stepping on each other's toes. Think of it like giving your AI a team of developers — it hands out assignments, watches over them, and gets notified when they're done.

**The human doesn't use Orca directly.** The human talks to their AI agent ("build me a payment system"), and the AI agent decides to use Orca to break the work into pieces and farm them out.

---

## Core Concepts

### Workers

A **worker** is an AI agent that's been given a task. When the orchestrator spawns a worker, Orca:

1. Creates a separate copy of the code (a **git worktree**) so the worker has its own sandbox
2. Opens a new terminal window (inside **tmux**) for the worker to run in
3. Launches the agent with the task description
4. Starts recording everything the agent does

Each worker gets a short, random name (like `fox` or `ace`) for easy reference.

### Orchestrators

The **orchestrator** is the AI agent that's in charge — the one the human is talking to. It reads the Orca skill, learns how to use the CLI, and then:

- Spawns workers for sub-tasks
- Gets notified when they finish
- Reviews their output
- Decides what happens next

The human tells the orchestrator what they want; the orchestrator figures out how to parallelize it.

### The Daemon

The **daemon** is a background process that quietly watches all workers. It does three things:

1. **Detects completion** — terminal closes or agent goes idle
2. **Detects blockers** — agent is stuck on a question or prompt
3. **Sends notifications** — tells the orchestrator about these events

Nobody interacts with the daemon directly. It starts automatically and runs silently.

---

## Lifecycle

### Spawning a Worker

When the orchestrator runs `orca spawn "fix the login bug"`:

| Step | What Happens |
|---|---|
| **Safety checks** | Verify worker count limit (default: 10) and depth limit (default: 3) aren't exceeded |
| **Code isolation** | Create a git worktree on a fresh branch — the worker can edit files freely without conflicts |
| **Terminal setup** | Open a new tmux window, named with the worker's name and depth emoji |
| **Agent launch** | Start the AI agent with the task description in autonomous mode |
| **Logging** | Capture all terminal output to a log file for later review |
| **State tracking** | Save worker info (name, task, status, etc.) to the state file |
| **Daemon activation** | Start the background daemon if it isn't running yet |

If anything fails, Orca cleans up after itself — removes the worktree, closes the window, and returns an error.

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
  orca kill fox    -- close and free resources
```

The orchestrator then decides: review logs, send a follow-up, kill, or report back to the human.

### Cleanup

| Command | Behavior |
|---|---|
| `orca kill <name>` | Closes the terminal, removes the worktree, removes from state |
| `orca gc` | Bulk cleanup of all finished/dead workers |
| `orca killall --mine` | Kills all workers belonging to the calling orchestrator |

Sub-workers (L1+) are expected to clean up after themselves before reporting done. The top-level orchestrator lets the human decide when to kill workers — they might want to inspect logs or cherry-pick branches first.

---

## Use Cases

In all of these scenarios, the **human** tells their AI agent what to do, and the **AI agent** uses Orca to parallelize the work. The human doesn't run Orca commands — the AI does.

### Divide and Conquer

> *"Build me a user registration system."*

The orchestrator breaks it into independent pieces:

```bash
orca spawn "implement the user registration API" -b cc -d ~/proj --orchestrator cc
orca spawn "build the registration form UI"      -b cc -d ~/proj --orchestrator cc
orca spawn "write tests for the auth module"     -b cx -d ~/proj --orchestrator cc
```

All three run simultaneously. As each finishes, the orchestrator reviews and reports back.

### Parallel Exploration

> *"The database query on the dashboard is slow, fix it."*

The orchestrator tries multiple approaches in parallel:

```bash
orca spawn "optimize using database indexes"       -b cc -d ~/proj --orchestrator cc
orca spawn "optimize using a caching layer"        -b cc -d ~/proj --orchestrator cc
orca spawn "optimize by denormalizing the schema"  -b cx -d ~/proj --orchestrator cc
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
Claude Code session (L0 orchestrator)
  └── worker "ace" (L1) — "implement payment system"
        ├── sub-worker "fig" (L2) — "build Stripe integration"
        ├── sub-worker "kai" (L2) — "build payment form"
        └── sub-worker "sol" (L2) — "write payment tests"
```

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

Workers at max depth can still do their own work — they just can't spawn further sub-workers.

---

## Notifications

| Orchestrator | Mechanism |
|---|---|
| **Claude Code / Codex** | Message typed directly into the orchestrator's tmux pane |
| **Cursor** | Message sent to tmux pane 3 times (Cursor's input is less reliable) |
| **OpenClaw** | System event via `openclaw` CLI, with optional Slack routing |
| **None** | No notifications — check manually with `orca list` |

---

## Lifecycle Hooks

Orca can install lifecycle hooks into **Claude Code** and **Codex** — small scripts that automatically run when an agent stops, calling `orca report --event done`. This is the most reliable way for workers to signal completion.

**Cursor** doesn't support hooks, so Orca appends reporting instructions directly to the task prompt.

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

---

## Visual Language

Orca uses sea creature emojis to show hierarchy depth at a glance:

| Emoji | Level | Role |
|---|---|---|
| 🐋 | L0 | Top-level boss (whale) |
| 🐳 | L1 | Direct worker (baby whale) |
| 🐬 | L2 | Sub-worker (dolphin) |
| 🐟 | L3 | Deep sub-worker (fish) |
| 🦐 | L4+ | Deepest level (shrimp) |

```
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
