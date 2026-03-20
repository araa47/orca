# Orca Setup Guide

Complete guide to installing Orca and configuring your AI agents to use it.

## 1. Install Orca

### Prerequisites

- [Rust toolchain](https://rustup.rs/) (1.85+)
- [tmux](https://github.com/tmux/tmux) (3.0+)
- At least one AI coding agent: [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex), or [Cursor](https://cursor.com)

### Install via Cargo

```bash
cargo install --git https://github.com/araa47/orca
```

> `cargo install` places the binary in `~/.cargo/bin/`. If that's not on your `PATH`, add
> `export PATH="$HOME/.cargo/bin:$PATH"` to your shell profile.

Pre-built binaries may also be available on the [Releases](https://github.com/araa47/orca/releases) page.

## 2. Install the Agent Skill

The skill teaches your AI agent how to use Orca's CLI. Install it globally so every agent can discover it:

```bash
npx skills add araa47/orca --skill orca -g -y
```

Or install manually:

```bash
git clone https://github.com/araa47/orca.git /tmp/orca
mkdir -p ~/.agents/skills
cp -r /tmp/orca/skills/orca ~/.agents/skills/orca
```

### For OpenClaw

```bash
mkdir -p ~/.openclaw/skills
cp -r /tmp/orca/skills/orca ~/.openclaw/skills/orca
```

## 3. Install Lifecycle Hooks

Hooks let Orca detect when workers finish. Safe to run multiple times:

```bash
orca hooks install
```

## 4. Verify

```bash
orca --help            # shows all commands
orca daemon status     # daemon info
orca list              # should say "No workers."
```

## 5. Tell Your Agents About Orca (Critical Step)

Installing the binary and skill is not enough. For agents to **reliably** discover and use Orca, you must mention it in your project's agent instruction file. Without this, agents may not know Orca is available or may not think to delegate work to parallel workers.

Add a section like the examples below to the appropriate file in **each repo** where you want agents to use Orca.

### Claude Code -- add to `CLAUDE.md`

```markdown
## Orca (Parallel Agent Orchestration)

This project has Orca installed for parallel task execution. When facing tasks that
can be broken into independent pieces, use `orca spawn` to delegate work to parallel
workers instead of doing everything sequentially.

- Use `orca spawn "<task>" -b cc -d . --orchestrator cc` to spawn workers
- Use `orca list` to check worker status
- Use `orca logs <name>` to review worker output
- Use `orca kill <name>` to clean up finished workers
- After spawning workers, stop and wait -- the daemon notifies you when they finish
- See the orca skill for full CLI reference
```

### Codex / Cursor / Generic Agents -- add to `AGENTS.md`

```markdown
## Orca (Parallel Agent Orchestration)

Orca is available for spawning parallel AI workers. Each worker runs in its own git
worktree and tmux window. Delegate independent sub-tasks to workers instead of doing
everything sequentially.

Key commands:
- `orca spawn "<task>" -b cx -d . --orchestrator cx` -- spawn a worker
- `orca list` -- check status of all workers
- `orca logs <name>` -- view worker output
- `orca steer <name> "<msg>"` -- send follow-up instructions
- `orca kill <name>` -- kill a worker and clean up
- After spawning, wait for the daemon to notify you when workers finish
```

### OpenClaw -- add to `TOOLS.md`

> **Important:** Without Orca instructions in `TOOLS.md`, OpenClaw will default to using
> ACP-based agents instead of spawning local tmux workers via Orca.

```markdown
## Orca (Parallel Agent Orchestration)

Use Orca to spawn and manage parallel AI coding workers. Delegate sub-tasks to
workers that run in isolated git worktrees.

- `orca spawn "<task>" -b cc -d <project-dir> --orchestrator openclaw --reply-channel slack --reply-to <target>`
- `orca list` / `orca logs <name>` / `orca kill <name>`
- After spawning, stop and wait -- you'll receive a system event when workers finish
- When notified of completion, review logs, summarize results, and message the user directly
```

### Why This Step Matters

AI agents only use tools they know about. The skill file teaches them *how* to use Orca, but the project instruction file tells them *when* to use it and makes delegation a first-class strategy. Without it, agents default to sequential execution even when parallel work would be faster. For OpenClaw, without explicit Orca instructions in `TOOLS.md`, it will default to ACP-based agents rather than local tmux workers.

### Manual Takeover via tmux

A key advantage of Orca over cloud-based agent orchestration: every worker runs in a real tmux window on your machine. You can attach to any worker's pane at any time to inspect state, debug issues, fix something by hand, or take over where the agent left off. Run `tmux list-windows` to see all active workers, or use `orca list` and then attach to the relevant tmux session.

You can also add workflow-specific guidance like:
- "For tasks with 3+ independent components, always use Orca to parallelize"
- "Spawn a researcher worker before starting implementation to gather context"
- "Use the sprint-team skill for larger features"

## Optional: Start the Daemon Automatically

The daemon auto-starts on first `orca spawn`, but you can start it at login:

**Shell profile** (`~/.zshrc` or `~/.bashrc`):

```bash
orca daemon start 2>/dev/null
```

**macOS launchd** -- create `~/Library/LaunchAgents/com.orca.daemon.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.orca.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>-c</string>
        <string>orca daemon start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
</dict>
</plist>
```

Then: `launchctl load ~/Library/LaunchAgents/com.orca.daemon.plist`
