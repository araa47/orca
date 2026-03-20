//! Orchestrator wake-up strategies.

use crate::config::canonical_backend;
use crate::state::{Worker, get_worker};
use crate::tmux::{run_out, send_keys};

fn routing_block(worker: &Worker) -> String {
    if worker.reply_channel.is_empty() {
        return String::new();
    }
    let mut parts = vec![format!("  channel: {}", worker.reply_channel)];
    if !worker.reply_to.is_empty() {
        parts.push(format!("  target: {}", worker.reply_to));
    }
    if !worker.reply_thread.is_empty() {
        parts.push(format!("  thread-id: {}", worker.reply_thread));
    }
    let routing = parts.join("\n");

    let mut cmd_parts = vec![
        "openclaw".to_string(),
        "message".to_string(),
        "send".to_string(),
        "--channel".to_string(),
        worker.reply_channel.clone(),
    ];
    if !worker.reply_to.is_empty() {
        cmd_parts.push("--target".to_string());
        cmd_parts.push(worker.reply_to.clone());
    }
    if !worker.reply_thread.is_empty() {
        cmd_parts.push("--thread-id".to_string());
        cmd_parts.push(worker.reply_thread.clone());
    }
    cmd_parts.push("--message".to_string());
    cmd_parts.push("<summary>".to_string());
    let cmd_str = shell_join(&cmd_parts);

    format!(
        "\n\nRouting:\n{routing}\n\n\
         ACTION REQUIRED:\n\
         1. Review the output with: orca logs {name}\n\
         2. Summarize the output (include any PR links).\n\
         3. Send the summary via: {cmd_str}\n\
         4. Do NOT reply in-session — the user won't see it. Use openclaw message send.\n\
         5. Kill the worker: orca kill {name}",
        name = worker.name,
    )
}

/// Join command parts into a shell-safe string (simple quoting for args with spaces).
fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| {
            if p.contains(' ')
                || p.contains('"')
                || p.contains('\'')
                || p.contains('<')
                || p.contains('>')
            {
                format!("'{}'", p.replace('\'', "'\\''"))
            } else {
                p.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn wake_message(worker: &Worker) -> String {
    let mut msg = format!(
        "ORCA: worker {} ({}) finished.\n\
         \x20 orca logs {}    -- review output\n\
         \x20 orca steer {}   -- send follow-up\n\
         \x20 orca kill {}    -- close and free resources",
        worker.name, worker.backend, worker.name, worker.name, worker.name,
    );
    msg.push_str(&routing_block(worker));
    msg
}

fn stuck_message(worker: &Worker, label: &str, snippet: &str) -> String {
    let mut msg = format!(
        "ORCA: worker {} ({}) is stuck — {label}\n\
         \x20 orca logs {}    -- see full output\n\
         \x20 orca steer {} \"<response>\"  -- unblock it\n\
         \x20 orca kill {}    -- give up and kill\n\
         ---\n{snippet}",
        worker.name, worker.backend, worker.name, worker.name, worker.name,
    );
    msg.push_str(&routing_block(worker));
    msg
}

/// Find the correct pane to deliver notifications to.
fn resolve_delivery_target(worker: &Worker) -> String {
    if !worker.spawned_by.is_empty()
        && let Some(parent) = get_worker(&worker.spawned_by)
        && !parent.pane_id.is_empty()
    {
        return parent.pane_id.clone();
    }
    worker.orchestrator_pane.clone()
}

async fn deliver(worker: &Worker, msg: &str) {
    let raw_orch = &worker.orchestrator;
    if raw_orch == "none" || raw_orch.is_empty() {
        return;
    }

    let orch = if raw_orch != "openclaw" {
        canonical_backend(raw_orch).to_string()
    } else {
        raw_orch.clone()
    };

    match orch.as_str() {
        "claude" | "codex" | "cursor" => {
            let target = resolve_delivery_target(worker);
            if target.is_empty() {
                return;
            }
            let repeats = if orch == "cursor" { 3 } else { 1 };
            send_keys(&target, msg, true, true, 150, repeats).await;
        }
        "openclaw" => {
            if !worker.spawned_by.is_empty() {
                let target = resolve_delivery_target(worker);
                if !target.is_empty() {
                    let mut repeats = 1;
                    if let Some(parent) = get_worker(&worker.spawned_by)
                        && canonical_backend(&parent.backend) == "cursor"
                    {
                        repeats = 3;
                    }
                    send_keys(&target, msg, true, true, 150, repeats).await;
                    return;
                }
            }
            let (rc, _, stderr) = run_out(&[
                "openclaw", "system", "event", "--text", msg, "--mode", "now",
            ])
            .await;
            if rc != 0 {
                eprintln!("openclaw system event failed: {}", stderr.trim());
            }
        }
        _ => {}
    }
}

/// Send a completion notification to the orchestrator.
pub async fn wake_orchestrator(worker: &Worker) {
    deliver(worker, &wake_message(worker)).await;
}

/// Escalate a complex blocker to the orchestrator.
pub async fn notify_stuck(worker: &Worker, label: &str, snippet: &str) {
    deliver(worker, &stuck_message(worker, label, snippet)).await;
}

fn warn_message(worker: &Worker, reason: &str) -> String {
    let mut msg = format!(
        "ORCA: worker {} ({}) may be done or stalled — {reason}\n\
         \x20 This is a soft signal; the worker may still be working.\n\
         \x20 orca logs {}    -- inspect output\n\
         \x20 orca steer {} \"<follow-up>\"  -- nudge it\n\
         \x20 orca kill {}    -- give up and kill",
        worker.name, worker.backend, worker.name, worker.name, worker.name,
    );
    msg.push_str(&routing_block(worker));
    msg
}

/// Warn the orchestrator about a potentially stalled worker.
pub async fn warn_orchestrator(worker: &Worker, reason: &str) {
    deliver(worker, &warn_message(worker, reason)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_worker(name: &str, backend: &str) -> Worker {
        Worker {
            name: name.to_string(),
            backend: backend.to_string(),
            task: "test task".to_string(),
            dir: "/tmp/test".to_string(),
            workdir: "/tmp/test".to_string(),
            base_branch: "main".to_string(),
            orchestrator: "cc".to_string(),
            orchestrator_pane: "%0".to_string(),
            session_id: String::new(),
            reply_channel: String::new(),
            reply_to: String::new(),
            reply_thread: String::new(),
            pane_id: "%1".to_string(),
            depth: 0,
            spawned_by: String::new(),
            layout: "window".to_string(),
            status: "running".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            last_event_at: String::new(),
            done_reported: false,
            process_exited: false,
        }
    }

    fn make_worker_with_orchestrator(name: &str, backend: &str, orchestrator: &str) -> Worker {
        let mut w = make_worker(name, backend);
        w.orchestrator = orchestrator.to_string();
        w
    }

    #[test]
    fn wake_message_contains_worker_name() {
        let w = make_worker("alpha", "claude");
        let msg = wake_message(&w);
        assert!(msg.contains("alpha"));
        assert!(msg.contains("claude"));
        assert!(msg.contains("orca logs alpha"));
        assert!(msg.contains("orca steer alpha"));
        assert!(msg.contains("orca kill alpha"));
    }

    #[test]
    fn wake_message_starts_with_orca_prefix() {
        let w = make_worker("beta", "codex");
        let msg = wake_message(&w);
        assert!(msg.starts_with("ORCA: worker beta"));
    }

    #[test]
    fn stuck_message_contains_all_parts() {
        let w = make_worker("gamma", "cursor");
        let msg = stuck_message(&w, "permission prompt", "Do you trust? [y/N]");
        assert!(msg.contains("gamma"));
        assert!(msg.contains("cursor"));
        assert!(msg.contains("is stuck"));
        assert!(msg.contains("permission prompt"));
        assert!(msg.contains("Do you trust? [y/N]"));
        assert!(msg.contains("orca logs gamma"));
        assert!(msg.contains("orca steer gamma"));
        assert!(msg.contains("orca kill gamma"));
    }

    #[test]
    fn stuck_message_includes_separator() {
        let w = make_worker("delta", "claude");
        let msg = stuck_message(&w, "blocked", "some snippet");
        assert!(msg.contains("---\nsome snippet"));
    }

    #[test]
    fn wake_message_different_backends() {
        for backend in &["claude", "codex", "cursor"] {
            let w = make_worker("test", backend);
            let msg = wake_message(&w);
            assert!(
                msg.contains(backend),
                "Message should contain backend name {backend}"
            );
        }
    }

    #[test]
    fn resolve_delivery_target_uses_orchestrator_pane() {
        let w = make_worker("test", "claude");
        let target = resolve_delivery_target(&w);
        assert_eq!(target, "%0");
    }

    #[test]
    fn resolve_delivery_target_empty_orchestrator() {
        let mut w = make_worker("test", "claude");
        w.orchestrator_pane = String::new();
        w.spawned_by = String::new();
        let target = resolve_delivery_target(&w);
        assert_eq!(target, "");
    }

    // --- routing_block tests ---

    #[test]
    fn routing_block_empty_when_no_reply_channel() {
        let w = make_worker("test", "claude");
        assert_eq!(routing_block(&w), "");
    }

    #[test]
    fn routing_block_includes_channel() {
        let mut w = make_worker("test", "claude");
        w.reply_channel = "general".to_string();
        let block = routing_block(&w);
        assert!(block.contains("Routing:"));
        assert!(block.contains("channel: general"));
        assert!(block.contains("ACTION REQUIRED:"));
        assert!(block.contains("orca logs test"));
        assert!(block.contains("orca kill test"));
        assert!(block.contains("openclaw message send --channel general"));
    }

    #[test]
    fn routing_block_includes_target_and_thread() {
        let mut w = make_worker("rw", "claude");
        w.reply_channel = "dev".to_string();
        w.reply_to = "U123".to_string();
        w.reply_thread = "T456".to_string();
        let block = routing_block(&w);
        assert!(block.contains("target: U123"));
        assert!(block.contains("thread-id: T456"));
        assert!(block.contains("--target U123"));
        assert!(block.contains("--thread-id T456"));
    }

    #[test]
    fn routing_block_omits_target_when_empty() {
        let mut w = make_worker("rw", "claude");
        w.reply_channel = "dev".to_string();
        let block = routing_block(&w);
        assert!(!block.contains("target:"));
        assert!(!block.contains("--target"));
    }

    #[test]
    fn routing_block_omits_thread_when_empty() {
        let mut w = make_worker("rw", "claude");
        w.reply_channel = "dev".to_string();
        w.reply_to = "U123".to_string();
        let block = routing_block(&w);
        assert!(!block.contains("thread-id:"));
        assert!(!block.contains("--thread-id"));
    }

    // --- wake_message with routing ---

    #[test]
    fn wake_message_appends_routing_block() {
        let mut w = make_worker("alpha", "claude");
        w.reply_channel = "alerts".to_string();
        let msg = wake_message(&w);
        assert!(msg.contains("finished"));
        assert!(msg.contains("Routing:"));
        assert!(msg.contains("channel: alerts"));
    }

    #[test]
    fn wake_message_no_routing_without_channel() {
        let w = make_worker("alpha", "claude");
        let msg = wake_message(&w);
        assert!(!msg.contains("Routing:"));
    }

    // --- stuck_message with routing ---

    #[test]
    fn stuck_message_appends_routing_block() {
        let mut w = make_worker("gamma", "cursor");
        w.reply_channel = "ops".to_string();
        let msg = stuck_message(&w, "blocked", "snippet");
        assert!(msg.contains("is stuck"));
        assert!(msg.contains("Routing:"));
        assert!(msg.contains("channel: ops"));
    }

    // --- warn_message tests ---

    #[test]
    fn warn_message_contains_reason() {
        let w = make_worker("w1", "claude");
        let msg = warn_message(&w, "no output for 5m");
        assert!(msg.contains("may be done or stalled"));
        assert!(msg.contains("no output for 5m"));
        assert!(msg.contains("soft signal"));
        assert!(msg.contains("orca logs w1"));
        assert!(msg.contains("orca steer w1"));
        assert!(msg.contains("orca kill w1"));
    }

    #[test]
    fn warn_message_appends_routing_block() {
        let mut w = make_worker("w1", "claude");
        w.reply_channel = "alerts".to_string();
        let msg = warn_message(&w, "stalled");
        assert!(msg.contains("Routing:"));
        assert!(msg.contains("channel: alerts"));
    }

    #[test]
    fn warn_message_no_routing_without_channel() {
        let w = make_worker("w1", "claude");
        let msg = warn_message(&w, "stalled");
        assert!(!msg.contains("Routing:"));
    }

    // --- shell_join tests ---

    #[test]
    fn shell_join_simple_args() {
        let parts = vec![
            "openclaw".to_string(),
            "message".to_string(),
            "send".to_string(),
        ];
        assert_eq!(shell_join(&parts), "openclaw message send");
    }

    #[test]
    fn shell_join_quotes_special_chars() {
        let parts = vec![
            "cmd".to_string(),
            "--message".to_string(),
            "<summary>".to_string(),
        ];
        let joined = shell_join(&parts);
        assert!(joined.contains("'<summary>'"));
    }

    #[test]
    fn shell_join_quotes_spaces() {
        let parts = vec!["cmd".to_string(), "hello world".to_string()];
        let joined = shell_join(&parts);
        assert!(joined.contains("'hello world'"));
    }

    #[test]
    fn shell_join_escapes_single_quotes() {
        let parts = vec!["cmd".to_string(), "it's here".to_string()];
        let joined = shell_join(&parts);
        assert!(joined.contains("'it'\\''s here'"));
    }

    #[test]
    fn shell_join_quotes_double_quotes() {
        let parts = vec!["cmd".to_string(), "say \"hello\"".to_string()];
        let joined = shell_join(&parts);
        assert!(joined.contains("'say \"hello\"'"));
    }

    // --- deliver async tests ---

    #[tokio::test]
    async fn deliver_none_is_noop() {
        let w = make_worker_with_orchestrator("d1", "claude", "none");
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_empty_orchestrator_is_noop() {
        let w = make_worker_with_orchestrator("d2", "claude", "");
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_openclaw_calls_run_out() {
        let w = make_worker_with_orchestrator("d3", "claude", "openclaw");
        // openclaw binary doesn't exist in test env — fails gracefully
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_claude_sends_keys() {
        let w = make_worker_with_orchestrator("d4", "claude", "claude");
        // tmux not running in test env — send_keys fails silently
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_codex_sends_keys() {
        let w = make_worker_with_orchestrator("d5", "codex", "codex");
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_cursor_sends_keys() {
        let w = make_worker_with_orchestrator("d6", "cursor", "cursor");
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_alias_cc_resolves_to_claude() {
        let w = make_worker_with_orchestrator("d7", "claude", "cc");
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_unknown_orchestrator_is_noop() {
        let w = make_worker_with_orchestrator("d8", "claude", "unknown_orch");
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_empty_target_skips_send_keys() {
        let mut w = make_worker_with_orchestrator("d9", "claude", "claude");
        w.orchestrator_pane = String::new();
        w.spawned_by = String::new();
        deliver(&w, "test message").await;
    }

    #[tokio::test]
    async fn deliver_openclaw_with_spawned_by_empty_target() {
        let mut w = make_worker_with_orchestrator("d10", "claude", "openclaw");
        w.spawned_by = "nonexistent_parent".to_string();
        // Parent doesn't exist in state — falls through to openclaw system event
        deliver(&w, "test message").await;
    }

    // --- wake_orchestrator / notify_stuck / warn_orchestrator end-to-end ---

    #[tokio::test]
    async fn wake_orchestrator_none_is_noop() {
        let w = make_worker_with_orchestrator("wo1", "claude", "none");
        wake_orchestrator(&w).await;
    }

    #[tokio::test]
    async fn wake_orchestrator_claude() {
        let w = make_worker_with_orchestrator("wo2", "claude", "claude");
        wake_orchestrator(&w).await;
    }

    #[tokio::test]
    async fn notify_stuck_none_is_noop() {
        let w = make_worker_with_orchestrator("ns1", "claude", "none");
        notify_stuck(&w, "blocked", "snippet").await;
    }

    #[tokio::test]
    async fn notify_stuck_claude() {
        let w = make_worker_with_orchestrator("ns2", "claude", "claude");
        notify_stuck(&w, "permission prompt", "Do you trust?").await;
    }

    #[tokio::test]
    async fn warn_orchestrator_none_is_noop() {
        let w = make_worker_with_orchestrator("wa1", "claude", "none");
        warn_orchestrator(&w, "no output").await;
    }

    #[tokio::test]
    async fn warn_orchestrator_claude() {
        let w = make_worker_with_orchestrator("wa2", "claude", "claude");
        warn_orchestrator(&w, "no output for 5m").await;
    }

    // --- resolve_delivery_target with spawned_by ---

    #[test]
    fn resolve_delivery_target_with_nonexistent_parent() {
        let mut w = make_worker("rt1", "claude");
        w.spawned_by = "nonexistent_parent_xyz".to_string();
        // Parent doesn't exist — falls back to orchestrator_pane
        let target = resolve_delivery_target(&w);
        assert_eq!(target, "%0");
    }

    #[test]
    fn resolve_delivery_target_spawned_by_empty_still_uses_orchestrator() {
        let mut w = make_worker("rt2", "claude");
        w.spawned_by = String::new();
        w.orchestrator_pane = "%5".to_string();
        let target = resolve_delivery_target(&w);
        assert_eq!(target, "%5");
    }
}
