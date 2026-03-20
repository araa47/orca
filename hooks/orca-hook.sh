#!/usr/bin/env bash
# Shared Orca hook handler — called by per-backend shims.
# Usage: orca-hook.sh <backend> <hook_event> [json_on_stdin]
#
# Translates backend-specific hook signals into `orca report` calls.
# The hook handler does NOT notify the orchestrator directly — it only
# appends events and lets the daemon handle delivery.
#
# If this worker still has active sub-workers (running or blocked in Orca state),
# `orca report --source hook --event done` is recorded as heartbeat instead — the hook
# still runs each turn; Orca only treats completion after delegates finish.
set -euo pipefail

BACKEND="${1:-claude}"
HOOK_EVENT="${2:-stop}"

# Read JSON payload from stdin (Claude Code sends it, Codex sends it as arg)
PAYLOAD=""
if [ ! -t 0 ]; then
    PAYLOAD=$(cat 2>/dev/null || true)
fi
# Codex passes JSON as first positional arg after our args
if [ -z "$PAYLOAD" ] && [ -n "${3:-}" ] && [ "${3:-}" != "_" ]; then
    PAYLOAD="$3"
fi

# Worker name from env (set by launcher) or try to infer from cwd
WORKER="${ORCA_WORKER_NAME:-}"
if [ -z "$WORKER" ]; then
    # Try to extract from .worktrees/<name> path
    case "$PWD" in
        */.worktrees/*)
            WORKER="${PWD##*/.worktrees/}"
            WORKER="${WORKER%%/*}"
            ;;
    esac
fi
if [ -z "$WORKER" ]; then
    # Also check cwd from JSON payload
    if [ -n "$PAYLOAD" ] && command -v jq &>/dev/null; then
        JSON_CWD=$(echo "$PAYLOAD" | jq -r '.cwd // empty' 2>/dev/null || true)
        case "$JSON_CWD" in
            */.worktrees/*)
                WORKER="${JSON_CWD##*/.worktrees/}"
                WORKER="${WORKER%%/*}"
                ;;
        esac
    fi
fi

[ -z "$WORKER" ] && exit 0

case "$BACKEND" in
    claude)
        case "$HOOK_EVENT" in
            stop)
                # Claude Stop hook: check if the assistant message indicates blocking
                if [ -n "$PAYLOAD" ] && command -v jq &>/dev/null; then
                    MSG=$(echo "$PAYLOAD" | jq -r '.last_assistant_message // empty' 2>/dev/null || true)
                    STOP_ACTIVE=$(echo "$PAYLOAD" | jq -r '.stop_hook_active // false' 2>/dev/null || true)
                    # Only report done if this isn't a stop-hook continuation loop
                    if [ "$STOP_ACTIVE" = "true" ]; then
                        exit 0
                    fi
                fi
                orca report --worker "$WORKER" --event done --source hook --message "claude stop" 2>/dev/null || true
                ;;
            notification)
                # Claude Notification hook: check type
                if [ -n "$PAYLOAD" ] && command -v jq &>/dev/null; then
                    NTYPE=$(echo "$PAYLOAD" | jq -r '.type // empty' 2>/dev/null || true)
                    NMSG=$(echo "$PAYLOAD" | jq -r '.message // empty' 2>/dev/null || true)
                    if [ "$NTYPE" = "permission_prompt" ] || [ "$NTYPE" = "elicitation_dialog" ]; then
                        orca report --worker "$WORKER" --event blocked --source hook \
                            --message "${NTYPE}: ${NMSG}" 2>/dev/null || true
                    fi
                fi
                ;;
        esac
        ;;
    codex)
        case "$HOOK_EVENT" in
            stop)
                # Codex notify: only report on agent-turn-complete
                if [ -n "$PAYLOAD" ] && command -v jq &>/dev/null; then
                    ETYPE=$(echo "$PAYLOAD" | jq -r '.type // empty' 2>/dev/null || true)
                    if [ "$ETYPE" != "agent-turn-complete" ]; then
                        exit 0
                    fi
                fi
                orca report --worker "$WORKER" --event done --source hook --message "codex turn complete" 2>/dev/null || true
                ;;
        esac
        ;;
    cursor)
        # Cursor has no official hooks — this path exists for future use
        case "$HOOK_EVENT" in
            stop)
                orca report --worker "$WORKER" --event done --source hook --message "cursor stop" 2>/dev/null || true
                ;;
        esac
        ;;
esac

exit 0
