# Orca Setup (one-time)

## Install

```bash
cargo install --git https://github.com/araa47/orca
```

This gives you the `orca` command globally. Requires the [Rust toolchain](https://rustup.rs/) and [tmux](https://github.com/tmux/tmux).

## Add the Skill to Your Agents

Clone the repo (if you haven't already):

```bash
git clone https://github.com/araa47/orca.git /tmp/orca
```

### All agents (global `.agents/skills`)

Copy the skill into `~/.agents/skills/` so every agent (Claude Code, Codex, Cursor, etc.) can discover it automatically:

```bash
mkdir -p ~/.agents/skills
cp -r /tmp/orca/skills/orca ~/.agents/skills/orca
```

### OpenClaw

Copy the skill into `~/.openclaw/skills/` so OpenClaw sessions can use it:

```bash
mkdir -p ~/.openclaw/skills
cp -r /tmp/orca/skills/orca ~/.openclaw/skills/orca
```

## Install hooks

Orca uses hooks to detect when workers finish. This wires into Claude Code's stop hook and Codex's notify system. Cursor has no native hooks — orca injects reporting instructions into the prompt instead.

```bash
orca hooks install
```

This is safe to run multiple times — it merges orca hooks into your existing settings without overwriting anything.

To remove:

```bash
orca hooks uninstall
```

## Start the daemon

The daemon watches worker panes and sends notifications when they finish or get stuck. It auto-starts on first `orca spawn`, but you can also start it explicitly:

```bash
orca daemon start
```

To keep it always running, add to your shell profile (e.g. `~/.zshrc`):

```bash
orca daemon start 2>/dev/null
```

Or as a launchd service on macOS — create `~/Library/LaunchAgents/com.orca.daemon.plist`:

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

## Verify

```bash
orca daemon status   # should say "Daemon running (pid=...)"
orca list            # should say "No workers." if nothing is running
orca --help          # shows all commands
```
