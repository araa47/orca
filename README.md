<p align="center">
  <img src="https://em-content.zobj.net/source/apple/391/whale_1f40b.png" width="80" />
</p>

<h1 align="center">Orca</h1>

<p align="center">
  <strong>An agent orchestrator — lets AI coding agents spawn and manage parallel workers.</strong><br/>
  Each worker runs in its own git worktree and tmux window. No shared state, no conflicts.
</p>

<p align="center">
  <a href="https://github.com/araa47/orca/actions"><img src="https://img.shields.io/github/actions/workflow/status/araa47/orca/on-pr.yml?branch=main&style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/araa47/orca/releases"><img src="https://img.shields.io/github/v/release/araa47/orca?style=flat-square&color=blue" alt="Release"></a>
</p>

> **Note:** This is a hobby / personal project. Contributions and feedback welcome, but expect rough edges.

---

## What Is Orca?

Orca is a CLI tool **designed to be used by AI coding agents**, not humans directly. You talk to your agent, and it uses Orca behind the scenes to break work into pieces, farm them out to parallel workers, and create swarms of agents working simultaneously.

The primary use case is with [**OpenClaw**](https://github.com/openclaw/openclaw) as the L0 orchestrator — you delegate tasks from any OpenClaw channel (Slack, WhatsApp, Telegram, Matrix, Discord, etc.) and OpenClaw uses Orca to spawn workers, monitor progress, and report back results. You can also use it with Claude Code, Codex, or Cursor as workers — they **must** run inside a tmux session so Orca can auto-detect their pane for notification delivery.

Each worker gets its own **git worktree** (isolated code sandbox) and **tmux window** (isolated terminal). A background **daemon** monitors all workers, auto-handles simple prompts, escalates blockers, and notifies the orchestrator when work is done. Workers can also be orchestrators themselves, spawning sub-workers up to 3 levels deep.

> **Full setup guide:** [**SETUP.md**](skills/orca/references/SETUP.md) — install, configure, and wire up your agents.

---

## Quick Start

### Prerequisites

- [Git](https://git-scm.com/) (2.20+) — Orca uses worktrees and stash; auto-initializes non-repo directories (see [SPEC.md](SPEC.md))
- [Rust toolchain](https://rustup.rs/) (1.85+)
- [tmux](https://github.com/tmux/tmux) (3.0+)
- At least one AI coding agent: [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex), or [Cursor](https://cursor.com)

### 1. Install the Binary

```bash
cargo install --git https://github.com/araa47/orca
```

> `cargo install` places the binary in `~/.cargo/bin/`. If that's not on your `PATH`, add
> `export PATH="$HOME/.cargo/bin:$PATH"` to your shell profile.

Pre-built binaries may also be available on the [Releases](https://github.com/araa47/orca/releases) page.

### 2. Install the Agent Skill

The skill is what teaches your AI agent how to use Orca. Install it globally so every agent (Claude Code, Codex, Cursor, etc.) can discover it:

```bash
npx skills add araa47/orca --skill orca -g -y
```

Or install manually:

```bash
git clone https://github.com/araa47/orca.git /tmp/orca
mkdir -p ~/.agents/skills
cp -r /tmp/orca/skills/orca ~/.agents/skills/orca
```

### 3. Install Lifecycle Hooks

Hooks let Orca detect when workers finish. Safe to run multiple times — merges without overwriting:

```bash
orca hooks install
```

### 4. Verify

```bash
orca --help            # shows all commands
orca daemon status     # daemon info
orca list              # should say "No workers."
```

### 5. Tell Your Agents About Orca

**This is the most commonly missed step.** Installing the binary and skill is not enough — you need to mention Orca in your project's agent instruction file so agents know to delegate work to parallel workers.

Add an "Orca" section to the appropriate file in your repo:

| Agent | File to edit |
|---|---|
| **Claude Code** | `CLAUDE.md` in your project root |
| **Codex / Cursor / Generic** | `AGENTS.md` in your project root |
| **OpenClaw** | `TOOLS.md` in your project root |

**For OpenClaw** (`TOOLS.md`) — OpenClaw is the L0 orchestrator. It must explicitly use Orca instead of ACP for coding-agent requests:

```markdown
## AI Coding Agents

ALWAYS use the `orca` skill (`orca spawn`) to spawn coding agents.
Never run them directly via exec -- use orca workers so the user can attach to sessions.

- Do not use ACP for coding-agent requests
- Default CLI: Claude Code (-b cc), Codex (-b cx), Cursor (-b cu)
- Spawn pattern: `orca spawn "<task>" -b cc -d ~/path/to/repo --orchestrator openclaw --session-id <current-session-id> --spawned-by openclaw`
- After spawning, wait silently -- orca daemon notifies when done. Don't poll with `orca list` loops.
- Use `orca logs <name>`, `orca steer <name> "<msg>"`, `orca kill <name>` when done
```

**For Claude Code / Codex / Cursor** (`CLAUDE.md` or `AGENTS.md`) — these agents run as workers inside tmux. They identify themselves by their worker name (`ORCA_WORKER_NAME` env var):

```markdown
## Orca (Parallel Agent Orchestration)

This project has Orca installed for parallel task execution. When facing tasks
that can be broken into independent pieces, use `orca spawn` to delegate work
to parallel workers instead of doing everything sequentially.

You must be running inside a tmux session. Check ORCA_WORKER_NAME for your
worker name and pass it as --spawned-by.

- `orca spawn "<task>" -b cc -d . --orchestrator cc --spawned-by "$ORCA_WORKER_NAME"` to spawn workers
- `orca list` to check status, `orca logs <name>` to review output
- `orca kill <name>` to clean up finished workers
- After spawning, stop and wait -- the daemon notifies you when workers finish
```

**Bonus: manual takeover.** Because every worker runs in a real tmux window, you can attach to any worker's pane and take over manually at any time — inspect state, fix something by hand, or continue where the agent left off.

---

## How It Works

1. **`orca spawn`** creates a git worktree, opens a tmux window, and launches the AI agent
2. A **background daemon** watches all worker panes via tmux
3. When a worker **finishes**, the daemon notifies the orchestrator
4. When a worker is **stuck** on a prompt, the daemon auto-handles it or escalates
5. The orchestrator **reviews**, **steers**, or **kills** and spawns fresh

For the full design — lifecycle, notifications, isolation, stuck-worker handling — see [**SPEC.md**](SPEC.md).

---

## Supported Agents

### Backends (workers)

- **Claude Code** — `-b cc` or `-b claude` (binary: `claude`)
- **Codex** — `-b cx` or `-b codex` (binary: `codex`)
- **Cursor** — `-b cu` or `-b cursor` (binary: `agent` / `cursor agent`)

### Orchestrators

- **Claude Code** — `--orchestrator cc` — message to tmux pane
- **Codex** — `--orchestrator cx` — message to tmux pane
- **Cursor** — `--orchestrator cu` — message to tmux pane (sent 3x)
- **OpenClaw** — `--orchestrator openclaw` — notification via `openclaw system event`
- **Custom** — `--orchestrator <name>` — any agent with a gateway in `config.yaml` (e.g. Hermes)
- **None** — `--orchestrator none` — no notification, check with `orca list`

---

## Orca Is Just Plumbing

Orca handles the **mechanics** — spawning, isolation, monitoring, notifications. It does **not** decide how to split work, what roles agents play, or what workflow to follow.

- **Your prompt** drives the strategy. "Build a payment system, split it into parallel tasks" produces different results than "fix this one bug."
- **Optional workflow skills** like [`sprint-team`](skills/sprint-team/SKILL.md) can help structure work with roles (Researcher, Architect, Coder, Validator). But they're optional — the agent can figure out the split from your prompt alone.

---

## Usage Patterns

**Divide and conquer** — Break a task into pieces, spawn one worker per piece, review results.

**Parallel exploration** — Try multiple approaches simultaneously, pick the best.

**Fresh context** — Kill a finished worker, spawn a new one for the next step. Clean context windows lead to better results.

**Iterative refinement** — Steer a running worker with corrections instead of starting over.

**Hierarchical delegation** — A worker becomes an orchestrator, spawning its own sub-workers.

---

## CLI Reference

Full command reference lives in [`skills/orca/SKILL.md`](skills/orca/SKILL.md) — the same file AI agents read.

| Command | Description |
|---|---|
| `orca spawn "task"` | Spawn a new worker with a task |
| `orca list` | Show all workers and their status |
| `orca logs <name>` | View a worker's terminal output |
| `orca steer <name> "msg"` | Send a follow-up message to a worker |
| `orca kill <name>` | Kill a worker and clean up its worktree |
| `orca gc` | Clean up all finished/dead workers |
| `orca hooks install` | Install lifecycle hooks for Claude Code & Codex |

---

## Stuck Worker Handling

| Type | Examples | Action |
|---|---|---|
| **Auto-handled** | Workspace trust, permission prompts, y/n, rate limits | Daemon answers automatically |
| **Escalated** | Auth failures, missing credentials, ambiguous questions, repeated errors | Daemon notifies orchestrator |

---

## Configuration

Orca reads `~/.orca/config.yaml` (or `$ORCA_HOME/config.yaml`). See [`config.example.yaml`](config.example.yaml) for all options.

```bash
cp config.example.yaml ~/.orca/config.yaml
```

### Custom Orchestrator Gateways

Define shell command templates for agents not built into Orca (e.g. Hermes, webhooks):

```yaml
gateways:
  hermes:
    wake: 'hermes chat -q "{text}"'
    reply: 'hermes chat -q "Use the send_message tool to send to {channel}: {text}"'
```

Then spawn with `--orchestrator hermes`. Templates support `{text}`, `{worker}`, `{channel}`, `{target}`, `{thread}` placeholders.

### Environment Variables

Env vars override YAML values when set.

| Environment Variable | Default | Description |
|---|---|---|
| `ORCA_MAX_DEPTH` | `3` | Maximum nesting depth for sub-workers |
| `ORCA_MAX_WORKERS` | `10` | Maximum running workers per orchestrator |
| `ORCA_HOME` | `~/.orca` | Base directory for state, logs, and events |

---

## Docs

| Document | Description |
|---|---|
| [**SPEC.md**](SPEC.md) | Human-readable design spec — how Orca works, use cases, lifecycle |
| [**ARCHITECTURE.md**](ARCHITECTURE.md) | Technical architecture — module graph, data flow, runtime state |
| [**config.example.yaml**](config.example.yaml) | Example config — custom gateways, tuning knobs |
| [**skills/orca/SKILL.md**](skills/orca/SKILL.md) | CLI reference — what agents read to learn Orca |
| [**skills/sprint-team/SKILL.md**](skills/sprint-team/SKILL.md) | Optional sprint workflow with structured roles |
| [**CHANGELOG.md**](CHANGELOG.md) | Version history |

---

## Development

If you have [direnv](https://direnv.net/) installed, run `direnv allow` — it installs rustup, rustfmt, clippy, and cargo-nextest automatically. Otherwise see [CONTRIBUTING.md](CONTRIBUTING.md) for manual setup.

```bash
cargo build              # debug build
cargo nextest run        # run tests in parallel (preferred)
cargo test               # run tests without nextest
cargo fmt                # format code
cargo clippy             # lint
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for full guidelines.

---

## Disclaimer

This project is largely vibe-coded. There are no guarantees of correctness, stability, or
backwards compatibility. It works for the author's use cases, but it may not work for yours.
Please read the code and understand what it does before relying on it for anything important.
