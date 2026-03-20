#!/usr/bin/env bash
set -euo pipefail

# Orca hook uninstaller — removes ONLY orca hooks, preserves everything else.

ORCA_HOOK_MARKER="orca-hook.sh"

echo "=== Uninstalling Orca hooks ==="

# 1. Claude Code — remove only orca hook entries from settings.json
SETTINGS=~/.claude/settings.json
if [ -f "$SETTINGS" ] && command -v jq &>/dev/null; then
    if grep -q "$ORCA_HOOK_MARKER" "$SETTINGS" 2>/dev/null; then
        CLEANED=$(jq '
            if .hooks then
                .hooks |= with_entries(
                    .value |= map(
                        select(.hooks | all(.command | test("orca-hook\\.sh") | not))
                    )
                )
                | if (.hooks | to_entries | all(.value | length == 0))
                  then del(.hooks)
                  else .
                  end
            else . end
        ' "$SETTINGS")
        echo "$CLEANED" > "$SETTINGS"
        echo "  ok removed orca hooks from $SETTINGS"
    else
        echo "  skip no orca hooks found in $SETTINGS"
    fi
else
    echo "  skip $SETTINGS not found or jq missing"
fi

# 2. Codex — remove only orca notify line
CONFIG=~/.codex/config.toml
if [ -f "$CONFIG" ] && grep -q "$ORCA_HOOK_MARKER" "$CONFIG" 2>/dev/null; then
    sed -i.bak "/$ORCA_HOOK_MARKER/d" "$CONFIG" && rm -f "$CONFIG.bak"
    echo "  ok removed orca notify from $CONFIG"
else
    echo "  skip no orca hooks in codex config"
fi

# 3. Remove shared handler
rm -f ~/.local/bin/orca-hook.sh
echo "  ok removed ~/.local/bin/orca-hook.sh"

echo ""
echo "=== Done ==="
