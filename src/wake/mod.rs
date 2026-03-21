//! Orchestrator wake-up strategies.

use crate::state::{Worker, get_worker};
use crate::tmux::{run_out, send_keys};
use crate::types::{Backend, Orchestrator};

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
         4. Do NOT reply in-session — the user won't see it. Use openclaw message send.",
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
         \x20 orca steer {}   -- send follow-up",
        worker.name, worker.backend, worker.name, worker.name,
    );
    msg.push_str(&routing_block(worker));
    msg
}

fn stuck_message(worker: &Worker, label: &str, snippet: &str) -> String {
    let mut msg = format!(
        "ORCA: worker {} ({}) is stuck — {label}\n\
         \x20 orca logs {}    -- see full output\n\
         \x20 orca steer {} \"<response>\"  -- unblock it\n\
         ---\n{snippet}",
        worker.name, worker.backend, worker.name, worker.name,
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
    let orch = &worker.orchestrator;
    if *orch == Orchestrator::None {
        return;
    }

    match orch {
        Orchestrator::Backend(Backend::Claude)
        | Orchestrator::Backend(Backend::Codex)
        | Orchestrator::Backend(Backend::Cursor) => {
            let target = resolve_delivery_target(worker);
            if target.is_empty() {
                return;
            }
            let repeats = if *orch == Orchestrator::Backend(Backend::Cursor) {
                3
            } else {
                1
            };
            send_keys(&target, msg, true, true, 150, repeats).await;
        }
        Orchestrator::Backend(Backend::Openclaw) => {
            if !worker.spawned_by.is_empty() {
                let target = resolve_delivery_target(worker);
                if !target.is_empty() {
                    let mut repeats = 1;
                    if let Some(parent) = get_worker(&worker.spawned_by)
                        && parent.backend == Backend::Cursor
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
        Orchestrator::None => {}
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
         \x20 orca steer {} \"<follow-up>\"  -- nudge it",
        worker.name, worker.backend, worker.name, worker.name,
    );
    msg.push_str(&routing_block(worker));
    msg
}

/// Warn the orchestrator about a potentially stalled worker.
pub async fn warn_orchestrator(worker: &Worker, reason: &str) {
    deliver(worker, &warn_message(worker, reason)).await;
}

#[cfg(test)]
mod tests;
