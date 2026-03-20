#!/usr/bin/env bash
set -euo pipefail

# Orca hook installer — adds orca hooks to Claude Code and Codex WITHOUT
# overwriting existing user hooks.  Cursor has no native hooks, so it relies
# on the prompt-based report contract in spawn.py.
#
# Installs:
#   1. orca-hook.sh -> ~/.local/bin/orca-hook.sh  (shared handler)
#   2. Claude Code hooks MERGED into ~/.claude/settings.json
#   3. Codex notify in ~/.codex/config.toml (only if not already set)

ORCA_HOOK_MARKER="orca-hook.sh"

echo "=== Installing Orca hooks ==="

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK_SRC="$SCRIPT_DIR/orca-hook.sh"

if [ ! -f "$HOOK_SRC" ]; then
    echo "Error: orca-hook.sh not found at $HOOK_SRC" >&2
    exit 1
fi

mkdir -p ~/.local/bin

# ============================================================================
# 1. Install shared hook handler
# ============================================================================
echo "[1/3] Installing shared hook handler..."
cp "$HOOK_SRC" ~/.local/bin/orca-hook.sh
chmod +x ~/.local/bin/orca-hook.sh
echo "  ok orca-hook.sh -> ~/.local/bin/orca-hook.sh"

# ============================================================================
# 2. Claude Code hooks — merge into existing settings, don't overwrite
# ============================================================================
echo "[2/3] Claude Code hooks..."
if command -v claude &>/dev/null; then
    if ! command -v jq &>/dev/null; then
        echo "  error: jq is required to safely merge hooks — install jq first" >&2
    else
        SETTINGS=~/.claude/settings.json
        mkdir -p ~/.claude

        if [ ! -f "$SETTINGS" ]; then
            echo '{}' > "$SETTINGS"
        fi

        # Check if orca hooks are already installed
        if jq -e '.hooks' "$SETTINGS" &>/dev/null && \
           grep -q "$ORCA_HOOK_MARKER" "$SETTINGS" 2>/dev/null; then
            echo "  skip orca hooks already present"
        else
            # Build orca hook entries
            ORCA_STOP='{"matcher": "", "hooks": [{"type": "command", "command": "~/.local/bin/orca-hook.sh claude stop"}]}'
            ORCA_NOTIF='{"matcher": "", "hooks": [{"type": "command", "command": "~/.local/bin/orca-hook.sh claude notification"}]}'

            # Merge: append orca entries to existing arrays (or create them)
            MERGED=$(jq \
                --argjson stop "$ORCA_STOP" \
                --argjson notif "$ORCA_NOTIF" \
                '.hooks.Stop = ((.hooks.Stop // []) + [$stop]) | .hooks.Notification = ((.hooks.Notification // []) + [$notif])' \
                "$SETTINGS")
            echo "$MERGED" > "$SETTINGS"
            echo "  ok merged orca hooks into $SETTINGS"
        fi
    fi
else
    echo "  skip claude not found"
fi

# ============================================================================
# 3. Codex hooks — only set notify if not already pointing to orca
# ============================================================================
echo "[3/3] Codex hooks..."
if command -v codex &>/dev/null; then
    NOTIFY_LINE='notify = ["~/.local/bin/orca-hook.sh", "codex", "stop"]'
    CONFIG=~/.codex/config.toml

    if [ -f "$CONFIG" ] && grep -q "$ORCA_HOOK_MARKER" "$CONFIG"; then
        echo "  skip orca notify already present"
    elif [ -f "$CONFIG" ]; then
        if grep -q '^notify' "$CONFIG"; then
            echo "  warning: existing notify line found — not overwriting" >&2
            echo "  current: $(grep '^notify' "$CONFIG")" >&2
            echo "  to use orca, replace it with: $NOTIFY_LINE" >&2
        else
            TMP=$(mktemp)
            { echo "$NOTIFY_LINE"; echo ""; cat "$CONFIG"; } > "$TMP"
            mv "$TMP" "$CONFIG"
            echo "  ok added orca notify to $CONFIG"
        fi
    else
        mkdir -p ~/.codex
        echo "$NOTIFY_LINE" > "$CONFIG"
        echo "  ok created $CONFIG with orca notify"
    fi
else
    echo "  skip codex not found"
fi

echo ""
echo "=== Done ==="
echo "Hooks report via 'orca report'. The daemon handles orchestrator delivery."
