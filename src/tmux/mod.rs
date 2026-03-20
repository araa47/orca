//! Async tmux helpers.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use tokio::process::Command;
use tokio::time::{Duration, Instant};

use crate::config::{canonical_backend, tmux_session};
use crate::prompts::{detect_prompt, handle_simple_prompt};

// ---------------------------------------------------------------------------
// Low-level helpers
// ---------------------------------------------------------------------------

/// Run a command and return (exit_code, stdout, stderr).
///
/// Applies a 10-second timeout so that a hung subprocess (e.g. tmux
/// connecting to a stale socket) cannot block the caller forever.
pub(crate) async fn run_out(cmd: &[&str]) -> (i32, String, String) {
    let (program, args) = cmd.split_first().expect("cmd must not be empty");
    let child = match Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (-1, String::new(), String::new()),
    };
    match tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await {
        Ok(Ok(o)) => (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ),
        _ => (-1, String::new(), String::new()),
    }
}

/// Return the `-S <socket>` args if `$TMUX` is unset but a saved socket exists.
fn socket_args() -> Vec<String> {
    if std::env::var("TMUX").unwrap_or_default().is_empty()
        && let Some(sock) = crate::config::load_tmux_socket()
    {
        return vec!["-S".to_string(), sock];
    }
    Vec::new()
}

/// Run `tmux <args>` and return (exit_code, stdout).
pub async fn tmux(args: &[&str]) -> (i32, String) {
    let extra = socket_args();
    let mut cmd: Vec<&str> = vec!["tmux"];
    for a in &extra {
        cmd.push(a.as_str());
    }
    cmd.extend_from_slice(args);
    let (rc, out, _) = run_out(&cmd).await;
    (rc, out)
}

/// Strip display-only prefixes (depth emojis) from tmux window names.
///
/// Matches the Python `_normalize_window_name` — removes any leading
/// non-alphanumeric, non-`_-` characters so that "🐳fox" becomes "fox".
static NORMALIZE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^A-Za-z0-9_-]+").unwrap());

pub fn normalize_window_name(name: &str) -> String {
    NORMALIZE_RE.replace(name, "").into_owned()
}

/// Return `true` when tmux stderr indicates the target (session / window /
/// pane) does not exist rather than a hard failure.
#[allow(dead_code)]
pub fn tmux_target_missing(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    const MARKERS: &[&str] = &[
        "can't find target",
        "can't find pane",
        "can't find window",
        "can't find session",
        "can't find client",
        "is not a pane",
        "error connecting to",
        "no server running",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Return the current tmux pane_id (e.g. `%0`) or empty string if not in tmux.
pub fn detect_current_pane() -> String {
    if std::env::var("TMUX").unwrap_or_default().is_empty() {
        return String::new();
    }
    // Prefer TMUX_PANE which tmux sets to the pane_id of the process's own
    // pane.  display-message without -t returns the *active* pane of the
    // session, which may differ when a new window was just created.
    let pane_env = std::env::var("TMUX_PANE").unwrap_or_default();
    if !pane_env.is_empty() {
        return pane_env;
    }
    let extra = socket_args();
    let mut args: Vec<&str> = Vec::new();
    for a in &extra {
        args.push(a.as_str());
    }
    args.extend(["display-message", "-p", "#{pane_id}"]);
    let result = std::process::Command::new("tmux").args(&args).output();
    match result {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

pub async fn session_exists(session: &str) -> bool {
    let (rc, _) = tmux(&["has-session", "-t", session]).await;
    rc == 0
}

pub async fn create_session(session: &str) {
    tmux(&["new-session", "-d", "-s", session]).await;
}

pub async fn ensure_session(session: &str) {
    if !session_exists(session).await {
        create_session(session).await;
    }
}

// ---------------------------------------------------------------------------
// Window helpers
// ---------------------------------------------------------------------------

pub async fn window_exists(name: &str, session: &str) -> bool {
    let (_, out) = tmux(&["list-windows", "-t", session, "-F", "#{window_name}"]).await;
    out.lines().any(|l| normalize_window_name(l) == name)
}

#[allow(dead_code)]
pub async fn list_windows(session: &str) -> HashSet<String> {
    let (_, out) = tmux(&["list-windows", "-t", session, "-F", "#{window_name}"]).await;
    out.lines().map(normalize_window_name).collect()
}

/// Create a window and return its pane_id (e.g. `%42`).
pub async fn create_window(name: &str, workdir: &str, session: &str) -> String {
    let (_, out) = tmux(&[
        "new-window",
        "-t",
        session,
        "-n",
        name,
        "-c",
        workdir,
        "-P",
        "-F",
        "#{pane_id}",
    ])
    .await;
    out.trim().to_string()
}

#[allow(dead_code)]
pub async fn rename_window(target: &str, name: &str) {
    tmux(&["rename-window", "-t", target, name]).await;
}

// ---------------------------------------------------------------------------
// Keys / capture
// ---------------------------------------------------------------------------

pub async fn send_keys(
    target: &str,
    text: &str,
    enter: bool,
    literal: bool,
    enter_delay_ms: u64,
    enter_repeats: u32,
) {
    if literal {
        tmux(&["send-keys", "-t", target, "-l", "--", text]).await;
    } else {
        tmux(&["send-keys", "-t", target, "--", text]).await;
    }
    if enter {
        for _ in 0..enter_repeats {
            if enter_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(enter_delay_ms)).await;
            }
            tmux(&["send-keys", "-t", target, "Enter"]).await;
        }
    }
}

pub async fn capture_pane(target: &str, lines: u32) -> String {
    let neg = format!("-{lines}");
    let (_, out) = tmux(&["capture-pane", "-p", "-t", target, "-S", &neg]).await;
    out
}

// ---------------------------------------------------------------------------
// Kill / lifecycle
// ---------------------------------------------------------------------------

pub async fn kill_window(target: &str) {
    tmux(&["kill-window", "-t", target]).await;
}

/// Split `target` to create a new pane. Returns the new pane_id.
///
/// `horizontal=true`  -> split right (`-h`)
/// `horizontal=false` -> split down  (`-v`)
#[allow(dead_code)]
pub async fn create_pane(target: &str, workdir: &str, horizontal: bool, size: u32) -> String {
    let direction = if horizontal { "-h" } else { "-v" };
    let size_str = format!("{size}%");
    let (_, out) = tmux(&[
        "split-window",
        direction,
        "-l",
        &size_str,
        "-t",
        target,
        "-c",
        workdir,
        "-P",
        "-F",
        "#{pane_id}",
    ])
    .await;
    out.trim().to_string()
}

pub async fn kill_pane(pane_id: &str) {
    tmux(&["kill-pane", "-t", pane_id]).await;
}

/// Check if a pane_id (e.g. `%42`) still exists.
pub async fn pane_alive(pane_id: &str) -> bool {
    let (_, out) = tmux(&["list-panes", "-a", "-F", "#{pane_id}"]).await;
    out.lines().any(|l| l == pane_id)
}

/// Get the PID of the process running in a pane.
#[allow(dead_code)]
pub async fn get_pane_pid(pane_id: &str) -> Option<u32> {
    let (_, out) = tmux(&["display-message", "-p", "-t", pane_id, "#{pane_pid}"]).await;
    out.trim().parse().ok()
}

// ---------------------------------------------------------------------------
// Agent lifecycle detection
// ---------------------------------------------------------------------------

/// Poll pane until agent is past startup prompts.
///
/// Auto-handles simple prompts (trust, y/n, enter) via `prompts::detect_prompt`.
/// `target_override` allows targeting by pane_id for pane-layout workers.
pub async fn wait_for_running(
    name: &str,
    backend: &str,
    session: &str,
    timeout_secs: f64,
    target_override: &str,
) -> String {
    let target = if target_override.is_empty() {
        format!("{session}:{name}")
    } else {
        target_override.to_string()
    };

    let start = Instant::now();
    let timeout = Duration::from_secs_f64(timeout_secs);
    let mut prompts_handled: u32 = 0;
    const MAX_PROMPTS: u32 = 5;

    let shell_prompt_re = Regex::new(r"^\s*[❯$]\s*$").expect("valid regex");

    while start.elapsed() < timeout {
        let output = capture_pane(&target, 80).await;

        if is_agent_alive(&output, backend) {
            return "running".to_string();
        }

        let prompt = detect_prompt(&output);

        if prompt.kind == "simple" && prompts_handled < MAX_PROMPTS {
            handle_simple_prompt(&target, &prompt).await;
            prompts_handled += 1;
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        let stripped = output.trim_end();
        let last_line = stripped.rsplit('\n').next().unwrap_or("");
        if shell_prompt_re.is_match(last_line)
            && output.to_lowercase().contains("command not found")
        {
            return "error".to_string();
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    "timeout".to_string()
}

/// Check whether the agent process is alive (past startup prompts).
pub fn is_agent_alive(output: &str, backend: &str) -> bool {
    let canon = canonical_backend(backend);
    let lower = output.to_lowercase();

    // Guard: trust/permission prompt means agent isn't ready yet
    if lower.contains("trust this workspace") || lower.contains("workspace trust") {
        return false;
    }

    match canon {
        "claude" => {
            lower.contains("bypass permissions on")
                || (lower.contains("claude code") && !lower.contains("yes, i accept"))
                || output.contains('⏺')
        }
        "codex" => ["openai codex", "? for shortcuts", "context left"]
            .iter()
            .any(|s| lower.contains(s)),
        "cursor" => [
            "/ commands",
            "generating",
            "ctrl+c to stop",
            "auto-run all commands",
            "add a follow-up",
        ]
        .iter()
        .any(|s| lower.contains(s)),
        _ => false,
    }
}

/// Check if the agent has completed its task and is idle at the prompt.
pub fn is_agent_idle(output: &str, backend: &str) -> bool {
    if output.trim().is_empty() {
        return false;
    }

    let canon = canonical_backend(backend);
    let lower = output.to_lowercase();

    match canon {
        "claude" => is_claude_idle(output, &lower),
        "codex" => is_codex_idle(output, &lower),
        "cursor" => is_cursor_idle(&lower),
        _ => false,
    }
}

fn is_claude_idle(output: &str, lower: &str) -> bool {
    if !lower.contains("bypass permissions on") {
        return false;
    }

    let lines: Vec<&str> = output.trim().lines().collect();

    // Check thinking indicators in last 5 lines only
    const THINKING_PATTERNS: &[&str] = &[
        "thundering",
        "cogitating",
        "thinking",
        "contemplating",
        "pondering",
        "reflecting",
        "reasoning",
    ];
    let tail_start = lines.len().saturating_sub(5);
    let tail: String = lines[tail_start..].join("\n").to_lowercase();
    if THINKING_PATTERNS.iter().any(|p| tail.contains(p)) {
        return false;
    }

    // Find the last ❯ prompt line
    let last_prompt_idx = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| l.trim().starts_with('❯'))
        .map(|(i, _)| i);

    let Some(idx) = last_prompt_idx else {
        return false;
    };

    let prompt_text = lines[idx].trim().trim_start_matches('❯').trim();
    if !prompt_text.is_empty() {
        return false;
    }

    lines[..idx].iter().any(|l| {
        let s = l.trim();
        s.starts_with('❯') && s.len() > 3 // '❯' is 3 bytes in UTF-8
    }) || idx > 20 // substantial output means agent did work
}

fn is_codex_idle(_output: &str, lower: &str) -> bool {
    if !lower.contains("? for shortcuts") {
        return false;
    }
    // Only check last 5 lines for "thinking"
    let lines: Vec<&str> = lower.lines().collect();
    let start = lines.len().saturating_sub(5);
    !lines[start..].join("\n").contains("thinking")
}

fn is_cursor_idle(lower: &str) -> bool {
    if lower.contains("add a follow-up") {
        return true;
    }
    if lower.contains("/ commands") {
        return !lower.contains("generating") && !lower.contains("ctrl+c to stop");
    }
    false
}

// ---------------------------------------------------------------------------
// Convenience wrappers with default session
// ---------------------------------------------------------------------------

/// Convenience: `session_exists` with the default session.
#[allow(dead_code)]
pub async fn session_exists_default() -> bool {
    session_exists(tmux_session()).await
}

/// Convenience: `ensure_session` with the default session.
#[allow(dead_code)]
pub async fn ensure_session_default() {
    ensure_session(tmux_session()).await;
}

/// Convenience: `list_windows` with the default session.
#[allow(dead_code)]
pub async fn list_windows_default() -> HashSet<String> {
    list_windows(tmux_session()).await
}

#[cfg(test)]
mod tests;
