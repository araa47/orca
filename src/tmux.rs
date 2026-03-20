//! Async tmux helpers.

use std::collections::HashSet;

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
pub fn normalize_window_name(name: &str) -> String {
    let re = Regex::new(r"^[^A-Za-z0-9_-]+").expect("valid regex");
    re.replace(name, "").into_owned()
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
    let tail: String = lower
        .lines()
        .collect::<Vec<_>>()
        .iter()
        .rev()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    !tail.contains("thinking")
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
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // detect_current_pane — no tmux means empty string
    // -----------------------------------------------------------------------

    #[test]
    fn detect_current_pane_no_tmux() {
        // If TMUX is unset or empty, should return empty string
        // SAFETY: test-only, no concurrent env access expected
        unsafe { std::env::remove_var("TMUX") };
        let pane = detect_current_pane();
        assert_eq!(pane, "");
    }

    #[test]
    fn detect_current_pane_uses_tmux_pane_env() {
        // SAFETY: test-only, no concurrent env access expected
        unsafe {
            std::env::set_var("TMUX", "/tmp/tmux-1000/default,12345,0");
            std::env::set_var("TMUX_PANE", "%7");
        }
        let pane = detect_current_pane();
        assert_eq!(pane, "%7");
        // Clean up
        unsafe {
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
    }

    // -----------------------------------------------------------------------
    // normalize_window_name
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_strips_whale_emoji() {
        assert_eq!(normalize_window_name("🐋fox"), "fox");
    }

    #[test]
    fn normalize_strips_spouting_whale() {
        assert_eq!(normalize_window_name("🐳worker"), "worker");
    }

    #[test]
    fn normalize_strips_dolphin() {
        assert_eq!(normalize_window_name("🐬deep"), "deep");
    }

    #[test]
    fn normalize_strips_fish() {
        assert_eq!(normalize_window_name("🐟sub"), "sub");
    }

    #[test]
    fn normalize_strips_shrimp() {
        assert_eq!(normalize_window_name("🦐tiny"), "tiny");
    }

    #[test]
    fn normalize_preserves_plain_name() {
        assert_eq!(normalize_window_name("fox"), "fox");
    }

    #[test]
    fn normalize_preserves_underscores_dashes() {
        assert_eq!(normalize_window_name("my-worker_1"), "my-worker_1");
    }

    #[test]
    fn normalize_strips_multiple_emoji_prefix() {
        assert_eq!(normalize_window_name("🐋🐳name"), "name");
    }

    #[test]
    fn normalize_empty_string() {
        assert_eq!(normalize_window_name(""), "");
    }

    // -----------------------------------------------------------------------
    // tmux_target_missing
    // -----------------------------------------------------------------------

    #[test]
    fn target_missing_cant_find() {
        assert!(tmux_target_missing("can't find target: foo"));
    }

    #[test]
    fn target_missing_cant_find_pane() {
        assert!(tmux_target_missing("Can't find pane: %99"));
    }

    #[test]
    fn target_missing_cant_find_window() {
        assert!(tmux_target_missing("can't find window: worker"));
    }

    #[test]
    fn target_missing_cant_find_session() {
        assert!(tmux_target_missing("can't find session orca"));
    }

    #[test]
    fn target_missing_not_a_pane() {
        assert!(tmux_target_missing("abc is not a pane"));
    }

    #[test]
    fn target_missing_error_connecting() {
        assert!(tmux_target_missing(
            "error connecting to /tmp/tmux-1000/default"
        ));
    }

    #[test]
    fn target_missing_no_server_running() {
        assert!(tmux_target_missing(
            "no server running on /tmp/tmux-1000/default"
        ));
    }

    #[test]
    fn target_missing_unrelated_error() {
        assert!(!tmux_target_missing("ambiguous output"));
    }

    #[test]
    fn target_missing_empty_string() {
        assert!(!tmux_target_missing(""));
    }

    // -----------------------------------------------------------------------
    // is_agent_alive — Claude
    // -----------------------------------------------------------------------

    #[test]
    fn alive_claude_bypass_permissions() {
        let output = "╭──────────────────────────────────────────╮\n\
                       │ ✻ Welcome to Claude Code!                │\n\
                       │   bypass permissions on                  │\n\
                       ╰──────────────────────────────────────────╯\n\
                       ❯ ";
        assert!(is_agent_alive(output, "claude"));
        assert!(is_agent_alive(output, "cc")); // alias
    }

    #[test]
    fn alive_claude_with_circle() {
        let output = "Some output\n⏺ Working on something\nMore text";
        assert!(is_agent_alive(output, "claude"));
    }

    #[test]
    fn alive_claude_code_header() {
        let output = "Claude Code v1.2.3\nSome other text";
        assert!(is_agent_alive(output, "claude"));
    }

    #[test]
    fn alive_claude_blocked_by_trust_prompt() {
        let output = "Do you trust this workspace? [y/N]";
        assert!(!is_agent_alive(output, "claude"));
    }

    #[test]
    fn alive_claude_blocked_by_accept_prompt() {
        let output = "Claude Code v1.0\nYes, I accept the terms";
        assert!(!is_agent_alive(output, "claude"));
    }

    #[test]
    fn alive_claude_empty_output() {
        assert!(!is_agent_alive("", "claude"));
    }

    // -----------------------------------------------------------------------
    // is_agent_alive — Codex
    // -----------------------------------------------------------------------

    #[test]
    fn alive_codex_shortcuts() {
        let output = "OpenAI Codex\n? for shortcuts\nReady.";
        assert!(is_agent_alive(output, "codex"));
        assert!(is_agent_alive(output, "cx")); // alias
    }

    #[test]
    fn alive_codex_context_left() {
        let output = "Some text\n50% context left\nMore";
        assert!(is_agent_alive(output, "codex"));
    }

    #[test]
    fn alive_codex_not_ready() {
        let output = "Loading...\nPlease wait";
        assert!(!is_agent_alive(output, "codex"));
    }

    // -----------------------------------------------------------------------
    // is_agent_alive — Cursor
    // -----------------------------------------------------------------------

    #[test]
    fn alive_cursor_commands() {
        let output = "Welcome\n/ commands\nReady.";
        assert!(is_agent_alive(output, "cursor"));
        assert!(is_agent_alive(output, "cu")); // alias
    }

    #[test]
    fn alive_cursor_generating() {
        let output = "Generating code...\nctrl+c to stop";
        assert!(is_agent_alive(output, "cursor"));
    }

    #[test]
    fn alive_cursor_follow_up() {
        let output = "Done!\nAdd a follow-up";
        assert!(is_agent_alive(output, "cursor"));
    }

    #[test]
    fn alive_cursor_not_ready() {
        let output = "Loading workspace...";
        assert!(!is_agent_alive(output, "cursor"));
    }

    // -----------------------------------------------------------------------
    // is_agent_alive — unknown backend
    // -----------------------------------------------------------------------

    #[test]
    fn alive_unknown_backend() {
        assert!(!is_agent_alive("anything", "unknown_backend"));
    }

    // -----------------------------------------------------------------------
    // is_agent_idle — Claude
    // -----------------------------------------------------------------------

    #[test]
    fn idle_claude_with_completed_task() {
        // Simulates: agent did work, now showing empty prompt
        let mut lines = vec![
            "╭──────────────────────────────────────────╮",
            "│ ✻ Welcome to Claude Code!                │",
            "│   bypass permissions on                  │",
            "╰──────────────────────────────────────────╯",
            "❯ write tests for the config module",
            "",
            "⏺ I'll write tests for config.rs",
            "",
            "  Tests added successfully.",
            "",
        ];
        // Add enough lines to get past idx > 20 or have prior task
        lines.extend(std::iter::repeat_n("  ... more output ...", 15));
        lines.push("❯ ");
        let output = lines.join("\n");
        assert!(is_agent_idle(&output, "claude"));
    }

    #[test]
    fn idle_claude_still_thinking() {
        let output = "bypass permissions on\n\
                       ❯ do something\n\
                       ⏺ Working...\n\
                       Thinking\n\
                       ❯ ";
        assert!(!is_agent_idle(output, "claude"));
    }

    #[test]
    fn idle_claude_empty_output() {
        assert!(!is_agent_idle("", "claude"));
        assert!(!is_agent_idle("   \n  ", "claude"));
    }

    #[test]
    fn idle_claude_no_bypass_permissions() {
        let output = "Some output\n❯ did work\n❯ ";
        assert!(!is_agent_idle(output, "claude"));
    }

    #[test]
    fn idle_claude_prompt_has_text() {
        // If the last ❯ line has text after it, not idle (user is typing)
        let output = "bypass permissions on\n❯ do something\n❯ still typing";
        assert!(!is_agent_idle(output, "claude"));
    }

    #[test]
    fn idle_claude_no_prior_task() {
        // Only one empty prompt, no prior work done
        let output = "bypass permissions on\n❯ ";
        assert!(!is_agent_idle(output, "claude"));
    }

    // -----------------------------------------------------------------------
    // is_agent_idle — Codex
    // -----------------------------------------------------------------------

    #[test]
    fn idle_codex_ready() {
        let output = "OpenAI Codex\n? for shortcuts\nDone with task.";
        assert!(is_codex_idle(output, &output.to_lowercase()));
    }

    #[test]
    fn idle_codex_still_thinking() {
        let output = "? for shortcuts\nSome output\nthinking\nmore";
        assert!(!is_codex_idle(output, &output.to_lowercase()));
    }

    #[test]
    fn idle_codex_no_shortcuts_marker() {
        let output = "Some random output";
        assert!(!is_codex_idle(output, &output.to_lowercase()));
    }

    // -----------------------------------------------------------------------
    // is_agent_idle — Cursor
    // -----------------------------------------------------------------------

    #[test]
    fn idle_cursor_follow_up() {
        let lower = "done!\nadd a follow-up";
        assert!(is_cursor_idle(lower));
    }

    #[test]
    fn idle_cursor_commands_no_activity() {
        let lower = "/ commands\nsome output";
        assert!(is_cursor_idle(lower));
    }

    #[test]
    fn idle_cursor_generating() {
        let lower = "/ commands\ngenerating something";
        assert!(!is_cursor_idle(lower));
    }

    #[test]
    fn idle_cursor_ctrl_c_to_stop() {
        let lower = "/ commands\nctrl+c to stop";
        assert!(!is_cursor_idle(lower));
    }

    #[test]
    fn idle_cursor_no_markers() {
        let lower = "random output";
        assert!(!is_cursor_idle(lower));
    }

    // -----------------------------------------------------------------------
    // is_agent_idle — unknown backend
    // -----------------------------------------------------------------------

    #[test]
    fn idle_unknown_backend() {
        assert!(!is_agent_idle("anything", "unknown"));
    }

    // -----------------------------------------------------------------------
    // is_agent_alive — workspace trust blocks all backends
    // -----------------------------------------------------------------------

    #[test]
    fn alive_workspace_trust_blocks_all() {
        let output = "workspace trust dialog here";
        assert!(!is_agent_alive(output, "claude"));
        assert!(!is_agent_alive(output, "codex"));
        assert!(!is_agent_alive(output, "cursor"));
    }

    // -----------------------------------------------------------------------
    // run_out — async command execution
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_out_echo_hello() {
        let (rc, stdout, _) = run_out(&["echo", "hello"]).await;
        assert_eq!(rc, 0);
        assert_eq!(stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn run_out_true_returns_zero() {
        let (rc, _, _) = run_out(&["true"]).await;
        assert_eq!(rc, 0);
    }

    #[tokio::test]
    async fn run_out_false_returns_nonzero() {
        let (rc, _, _) = run_out(&["false"]).await;
        assert_ne!(rc, 0);
    }

    #[tokio::test]
    async fn run_out_captures_stderr() {
        let (rc, _, stderr) = run_out(&["sh", "-c", "echo err >&2; exit 1"]).await;
        assert_ne!(rc, 0);
        assert!(stderr.contains("err"));
    }

    #[tokio::test]
    async fn run_out_nonexistent_command() {
        let (rc, stdout, _) = run_out(&["__orca_nonexistent_cmd__"]).await;
        assert_eq!(rc, -1);
        assert!(stdout.is_empty());
    }

    #[tokio::test]
    async fn run_out_multi_arg_command() {
        let (rc, stdout, _) = run_out(&["printf", "%s-%s", "foo", "bar"]).await;
        assert_eq!(rc, 0);
        assert_eq!(stdout, "foo-bar");
    }

    // -----------------------------------------------------------------------
    // socket_args — TMUX env interaction
    // -----------------------------------------------------------------------

    #[test]
    fn socket_args_empty_when_tmux_set() {
        unsafe { std::env::set_var("TMUX", "/tmp/tmux-1000/default,12345,0") };
        let args = socket_args();
        assert!(args.is_empty());
        unsafe { std::env::remove_var("TMUX") };
    }

    #[test]
    fn socket_args_empty_when_no_saved_socket() {
        unsafe { std::env::remove_var("TMUX") };
        let args = socket_args();
        // Without a saved socket file, should return empty
        // (may or may not be empty depending on config, but should not panic)
        let _ = args;
    }

    // -----------------------------------------------------------------------
    // tmux() wrapper — runs without panicking even if tmux is absent
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn tmux_wrapper_returns_nonzero_on_bad_subcommand() {
        let (rc, _) = tmux(&["__nonexistent_subcommand__"]).await;
        // tmux is unlikely to have this subcommand; will error
        assert_ne!(rc, 0);
    }

    // -----------------------------------------------------------------------
    // session_exists / create / ensure — graceful failures without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn session_exists_returns_false_without_server() {
        let exists = session_exists("__orca_test_nonexistent__").await;
        assert!(!exists);
    }

    #[tokio::test]
    async fn ensure_session_does_not_panic() {
        // Even without a tmux server, ensure_session should not panic
        ensure_session("__orca_test_ensure__").await;
    }

    // -----------------------------------------------------------------------
    // window_exists / list_windows — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn window_exists_returns_false_without_server() {
        let exists = window_exists("worker", "__orca_test_sess__").await;
        assert!(!exists);
    }

    #[tokio::test]
    async fn list_windows_returns_empty_without_server() {
        let windows = list_windows("__orca_test_sess__").await;
        assert!(windows.is_empty());
    }

    // -----------------------------------------------------------------------
    // create_window — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_window_returns_empty_without_server() {
        let pane_id = create_window("test", "/tmp", "__orca_test_sess__").await;
        // Without a tmux server, pane_id will be empty
        assert!(pane_id.is_empty() || !pane_id.starts_with('%'));
    }

    // -----------------------------------------------------------------------
    // rename_window — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn rename_window_does_not_panic() {
        rename_window("__orca_test__:0", "newname").await;
    }

    // -----------------------------------------------------------------------
    // send_keys — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn send_keys_does_not_panic_literal() {
        send_keys("__orca_test__:0", "hello", false, true, 0, 1).await;
    }

    #[tokio::test]
    async fn send_keys_does_not_panic_with_enter() {
        send_keys("__orca_test__:0", "hello", true, false, 0, 1).await;
    }

    #[tokio::test]
    async fn send_keys_with_delay_and_repeats() {
        send_keys("__orca_test__:0", "hello", true, false, 10, 2).await;
    }

    #[tokio::test]
    async fn send_keys_no_enter() {
        send_keys("__orca_test__:0", "text", false, false, 0, 1).await;
    }

    // -----------------------------------------------------------------------
    // capture_pane — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn capture_pane_returns_empty_without_server() {
        let output = capture_pane("__orca_test__:0", 50).await;
        assert!(output.is_empty());
    }

    // -----------------------------------------------------------------------
    // kill_window / kill_pane — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn kill_window_does_not_panic() {
        kill_window("__orca_test__:0").await;
    }

    #[tokio::test]
    async fn kill_pane_does_not_panic() {
        kill_pane("%999999").await;
    }

    // -----------------------------------------------------------------------
    // create_pane — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_pane_returns_empty_without_server() {
        let pane = create_pane("__orca_test__:0", "/tmp", true, 50).await;
        assert!(pane.is_empty() || !pane.starts_with('%'));
    }

    #[tokio::test]
    async fn create_pane_vertical() {
        let pane = create_pane("__orca_test__:0", "/tmp", false, 30).await;
        assert!(pane.is_empty() || !pane.starts_with('%'));
    }

    // -----------------------------------------------------------------------
    // pane_alive — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn pane_alive_returns_false_without_server() {
        let alive = pane_alive("%999999").await;
        assert!(!alive);
    }

    // -----------------------------------------------------------------------
    // get_pane_pid — graceful without tmux server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_pane_pid_returns_none_without_server() {
        let pid = get_pane_pid("%999999").await;
        assert!(pid.is_none());
    }

    // -----------------------------------------------------------------------
    // wait_for_running — short timeout returns "timeout"
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn wait_for_running_times_out_quickly() {
        let status = wait_for_running("__orca_nowin__", "claude", "__orca_nosess__", 0.5, "").await;
        assert_eq!(status, "timeout");
    }

    #[tokio::test]
    async fn wait_for_running_with_target_override() {
        let status = wait_for_running(
            "__orca_nowin__",
            "claude",
            "__orca_nosess__",
            0.5,
            "%999999",
        )
        .await;
        assert_eq!(status, "timeout");
    }

    #[tokio::test]
    async fn wait_for_running_codex_timeout() {
        let status = wait_for_running("__orca_nowin__", "codex", "__orca_nosess__", 0.5, "").await;
        assert_eq!(status, "timeout");
    }

    // -----------------------------------------------------------------------
    // Convenience wrappers with default session
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn session_exists_default_does_not_panic() {
        let _ = session_exists_default().await;
    }

    #[tokio::test]
    async fn ensure_session_default_does_not_panic() {
        ensure_session_default().await;
    }

    #[tokio::test]
    async fn list_windows_default_does_not_panic() {
        let _ = list_windows_default().await;
    }

    // -----------------------------------------------------------------------
    // is_agent_alive — additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn alive_claude_alias_cc() {
        let output = "bypass permissions on\n❯ ";
        assert!(is_agent_alive(output, "cc"));
    }

    #[test]
    fn alive_codex_alias_cx() {
        let output = "OpenAI Codex\n? for shortcuts";
        assert!(is_agent_alive(output, "cx"));
    }

    #[test]
    fn alive_cursor_alias_cu() {
        let output = "/ commands\nReady";
        assert!(is_agent_alive(output, "cu"));
    }

    #[test]
    fn alive_cursor_auto_run() {
        let output = "auto-run all commands\nDone";
        assert!(is_agent_alive(output, "cursor"));
    }

    #[test]
    fn alive_codex_openai_header() {
        let output = "openai codex v0.1.2\nSome text";
        assert!(is_agent_alive(output, "codex"));
    }

    // -----------------------------------------------------------------------
    // is_agent_idle — additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn idle_claude_with_many_lines_and_empty_prompt() {
        let mut lines: Vec<String> = vec![
            "bypass permissions on".to_string(),
            "❯ write something long".to_string(),
        ];
        for i in 0..25 {
            lines.push(format!("  output line {i}"));
        }
        lines.push("❯ ".to_string());
        let output = lines.join("\n");
        assert!(is_agent_idle(&output, "claude"));
    }

    #[test]
    fn idle_claude_pondering() {
        let output = "bypass permissions on\n❯ task\npondering\n❯ ";
        assert!(!is_agent_idle(output, "claude"));
    }

    #[test]
    fn idle_claude_cogitating() {
        let output = "bypass permissions on\n❯ task\ncogitating\n❯ ";
        assert!(!is_agent_idle(output, "claude"));
    }

    #[test]
    fn idle_claude_contemplating() {
        let output = "bypass permissions on\n❯ task\ncontemplating\n❯ ";
        assert!(!is_agent_idle(output, "claude"));
    }

    #[test]
    fn idle_codex_via_is_agent_idle() {
        let output = "? for shortcuts\nDone.";
        assert!(is_agent_idle(output, "codex"));
    }

    #[test]
    fn idle_codex_via_is_agent_idle_alias() {
        let output = "? for shortcuts\nDone.";
        assert!(is_agent_idle(output, "cx"));
    }

    #[test]
    fn idle_cursor_via_is_agent_idle() {
        let output = "add a follow-up\nDone.";
        assert!(is_agent_idle(output, "cursor"));
    }

    #[test]
    fn idle_cursor_via_is_agent_idle_alias() {
        let output = "add a follow-up\nDone.";
        assert!(is_agent_idle(output, "cu"));
    }

    #[test]
    fn idle_unknown_backend_via_is_agent_idle() {
        assert!(!is_agent_idle("anything", "unknown_backend_xyz"));
    }

    // -----------------------------------------------------------------------
    // normalize_window_name — additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_only_emoji() {
        assert_eq!(normalize_window_name("🐋"), "");
    }

    #[test]
    fn normalize_number_start() {
        assert_eq!(normalize_window_name("42worker"), "42worker");
    }

    // -----------------------------------------------------------------------
    // tmux_target_missing — additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn target_missing_cant_find_client() {
        assert!(tmux_target_missing("can't find client /dev/pts/0"));
    }

    #[test]
    fn target_missing_case_insensitive() {
        assert!(tmux_target_missing("CAN'T FIND TARGET: FOO"));
    }
}
