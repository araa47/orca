//! Prompt detection and classification.
//!
//! Classifies pane output as containing a simple prompt (auto-handleable)
//! or a complex blocker (must escalate to orchestrator).

use std::sync::LazyLock;

use regex::Regex;

/// Classification of a detected prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptInfo {
    /// `"simple"`, `"complex"`, or `"none"`.
    pub kind: String,
    /// Human-readable description.
    pub label: String,
    /// Relevant lines from the pane output.
    pub snippet: String,
}

impl PromptInfo {
    fn simple(label: &str, snippet: String) -> Self {
        Self {
            kind: "simple".into(),
            label: label.into(),
            snippet,
        }
    }

    fn complex(label: &str, snippet: String) -> Self {
        Self {
            kind: "complex".into(),
            label: label.into(),
            snippet,
        }
    }

    fn none() -> Self {
        Self {
            kind: "none".into(),
            label: String::new(),
            snippet: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Complex-blocker patterns (compiled once)
// ---------------------------------------------------------------------------

struct ComplexPattern {
    label: &'static str,
    re: Regex,
}

// Removed: permission_denied (caused false positives from URL substrings like "403"
//          in tokens) and agent_question (too broad, matches normal agent output).
// These should come from explicit `orca report --event blocked` calls instead.
static COMPLEX_PATTERNS: LazyLock<Vec<ComplexPattern>> = LazyLock::new(|| {
    vec![
        ComplexPattern {
            label: "auth_failure",
            re: Regex::new(r"(?i)auth(?:entication|orization)?\s+(?:failed|error|denied|required)")
                .unwrap(),
        },
        ComplexPattern {
            label: "credentials_missing",
            re: Regex::new(r"(?i)(?:api[_ ]?key|token|credentials?|password|secret)\s+(?:not found|missing|required|invalid|expired)")
                .unwrap(),
        },
        ComplexPattern {
            label: "rate_limit",
            re: Regex::new(r"(?i)rate\s*limit(?:ed)?|too many requests|\b429\b|quota exceeded")
                .unwrap(),
        },
        ComplexPattern {
            label: "ssh_key",
            re: Regex::new(r"(?i)\bssh\b.{0,80}(?:key|permission|denied|host)").unwrap(),
        },
        ComplexPattern {
            label: "network_error",
            re: Regex::new(r"(?i)(?:connection|network)\s+(?:refused|timeout|error|failed)|ECONNREFUSED|ETIMEDOUT")
                .unwrap(),
        },
    ]
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn last_n_lines(output: &str, n: usize) -> String {
    let lines: Vec<&str> = output
        .trim()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

static YES_NO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[y/n\]|\[yes/no\]|continue\? \(y\)").unwrap());

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Classify pane output as simple prompt, complex blocker, or none.
pub fn detect_prompt(output: &str) -> PromptInfo {
    let lower = output.to_lowercase();
    let collapsed = WHITESPACE_RE.replace_all(&lower, " ");

    // --- Simple: Claude Code accept prompt ---
    if collapsed.contains("yes, i accept") && collapsed.contains("enter to confirm") {
        return PromptInfo::simple("Claude Code permission acceptance", last_n_lines(output, 5));
    }

    // --- Simple: Workspace trust ([a] Trust this workspace) ---
    if lower.contains("[a]") && lower.contains("trust") && lower.contains("[q]") {
        return PromptInfo::simple("Workspace trust prompt", last_n_lines(output, 5));
    }

    // --- Simple: Codex directory trust ---
    if collapsed.contains("do you trust the contents") {
        return PromptInfo::simple("Directory trust confirmation", last_n_lines(output, 5));
    }

    // --- Simple: Codex rate-limit / model-switch prompt ---
    if collapsed.contains("rate limit")
        && collapsed.contains("switch")
        && collapsed.contains("press enter")
    {
        return PromptInfo::simple("Codex model switch prompt", last_n_lines(output, 5));
    }

    // --- Simple: Cursor auto-run ---
    if collapsed.contains("auto-run")
        && collapsed.contains("shift+tab")
        && !collapsed.contains("turn off")
    {
        return PromptInfo::simple("Cursor auto-run prompt", last_n_lines(output, 5));
    }

    // --- Simple: Press enter to confirm or esc ---
    if collapsed.contains("press enter to confirm or esc") {
        return PromptInfo::simple("Press enter to confirm", last_n_lines(output, 5));
    }

    // --- Simple: y/n prompt ---
    if YES_NO_RE.is_match(&lower) {
        return PromptInfo::simple("Yes/No confirmation", last_n_lines(output, 5));
    }

    // --- Simple: generic press enter ---
    if collapsed.contains("press enter") {
        return PromptInfo::simple("Press enter to continue", last_n_lines(output, 5));
    }

    // --- Complex blockers ---
    for cp in COMPLEX_PATTERNS.iter() {
        if cp.re.is_match(&collapsed) {
            return PromptInfo::complex(cp.label, last_n_lines(output, 10));
        }
    }

    PromptInfo::none()
}

// ---------------------------------------------------------------------------
// Auto-handling
// ---------------------------------------------------------------------------

/// Send keys to auto-handle a simple prompt. Returns `true` if handled.
pub async fn handle_simple_prompt(target: &str, prompt: &PromptInfo) -> bool {
    use tokio::time::{Duration, sleep};

    let label = prompt.label.as_str();

    match label {
        "Claude Code permission acceptance" => {
            crate::tmux::tmux(&["send-keys", "-t", target, "Down"]).await;
            sleep(Duration::from_millis(500)).await;
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            sleep(Duration::from_millis(500)).await;
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            true
        }
        "Workspace trust prompt" => {
            crate::tmux::tmux(&["send-keys", "-t", target, "a"]).await;
            sleep(Duration::from_secs(1)).await;
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            true
        }
        "Directory trust confirmation" => {
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            true
        }
        "Codex model switch prompt" => {
            crate::tmux::tmux(&["send-keys", "-t", target, "2"]).await;
            sleep(Duration::from_millis(300)).await;
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            true
        }
        "Cursor auto-run prompt" => {
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            true
        }
        "Press enter to confirm" | "Press enter to continue" => {
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            true
        }
        "Yes/No confirmation" => {
            crate::tmux::tmux(&["send-keys", "-t", target, "y"]).await;
            sleep(Duration::from_millis(100)).await;
            crate::tmux::tmux(&["send-keys", "-t", target, "Enter"]).await;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- PromptInfo constructors ---

    #[test]
    fn prompt_info_none_fields() {
        let p = PromptInfo::none();
        assert_eq!(p.kind, "none");
        assert!(p.label.is_empty());
        assert!(p.snippet.is_empty());
    }

    #[test]
    fn prompt_info_simple_fields() {
        let p = PromptInfo::simple("test label", "snippet text".to_string());
        assert_eq!(p.kind, "simple");
        assert_eq!(p.label, "test label");
        assert_eq!(p.snippet, "snippet text");
    }

    #[test]
    fn prompt_info_complex_fields() {
        let p = PromptInfo::complex("auth_failure", "error output".to_string());
        assert_eq!(p.kind, "complex");
        assert_eq!(p.label, "auth_failure");
        assert_eq!(p.snippet, "error output");
    }

    #[test]
    fn prompt_info_equality() {
        let a = PromptInfo::simple("x", "y".to_string());
        let b = PromptInfo::simple("x", "y".to_string());
        assert_eq!(a, b);

        let c = PromptInfo::complex("x", "y".to_string());
        assert_ne!(a, c);
    }

    #[test]
    fn prompt_info_clone() {
        let a = PromptInfo::simple("label", "snip".to_string());
        let b = a.clone();
        assert_eq!(a, b);
    }

    // --- last_n_lines ---

    #[test]
    fn last_n_lines_basic() {
        let output = "line1\nline2\nline3\nline4\nline5";
        assert_eq!(last_n_lines(output, 3), "line3\nline4\nline5");
    }

    #[test]
    fn last_n_lines_fewer_than_n() {
        let output = "line1\nline2";
        assert_eq!(last_n_lines(output, 5), "line1\nline2");
    }

    #[test]
    fn last_n_lines_filters_blank_lines() {
        let output = "line1\n\n\nline2\n\nline3\n";
        assert_eq!(last_n_lines(output, 2), "line2\nline3");
    }

    #[test]
    fn last_n_lines_empty_input() {
        assert_eq!(last_n_lines("", 5), "");
    }

    #[test]
    fn last_n_lines_whitespace_only() {
        assert_eq!(last_n_lines("   \n  \n   ", 5), "");
    }

    #[test]
    fn last_n_lines_single_line() {
        assert_eq!(last_n_lines("hello", 3), "hello");
    }

    #[test]
    fn last_n_lines_zero_requested() {
        assert_eq!(last_n_lines("a\nb\nc", 0), "");
    }

    // --- detect_prompt existing tests ---

    #[test]
    fn detects_claude_accept() {
        let output = "Do you accept?\nYes, I accept\nPress enter to confirm";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Claude Code permission acceptance");
    }

    #[test]
    fn detects_workspace_trust() {
        let output = "[a] Trust this workspace\n[q] Quit";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Workspace trust prompt");
    }

    #[test]
    fn detects_directory_trust() {
        let output = "Do you trust the contents of this directory?";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Directory trust confirmation");
    }

    #[test]
    fn detects_codex_model_switch() {
        let output = "Rate limit reached. Switch to another model?\nPress enter to continue";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Codex model switch prompt");
    }

    #[test]
    fn detects_cursor_auto_run() {
        let output = "Enable auto-run? Press shift+tab to toggle";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Cursor auto-run prompt");
    }

    #[test]
    fn skips_cursor_auto_run_status_bar() {
        let output = "Auto-run all commands (shift+tab to turn off)";
        let info = detect_prompt(output);
        assert_ne!(info.label, "Cursor auto-run prompt");
    }

    #[test]
    fn detects_yes_no() {
        let output = "Proceed? [y/n]";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Yes/No confirmation");
    }

    #[test]
    fn detects_press_enter() {
        let output = "Press enter to continue";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Press enter to continue");
    }

    #[test]
    fn detects_auth_failure() {
        let output = "Error: authentication failed for user foo";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "auth_failure");
    }

    #[test]
    fn detects_rate_limit() {
        let output = "429 Too Many Requests";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "rate_limit");
    }

    #[test]
    fn rate_limit_429_word_boundary() {
        let output = "token abc4291def granted";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "none", "429 inside a word should not match");
    }

    #[test]
    fn ssh_key_bounded_gap() {
        let output = "ssh: could not load key";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "ssh_key");
    }

    #[test]
    fn ssh_key_rejects_huge_gap() {
        let long_filler = "x".repeat(100);
        let output = format!("ssh {} key failure", long_filler);
        let info = detect_prompt(&output);
        assert_ne!(
            info.label, "ssh_key",
            "gap >80 chars should not match ssh_key"
        );
    }

    #[test]
    fn permission_denied_no_longer_detected() {
        let output = "Error: permission denied for /etc/shadow";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "none", "permission_denied pattern was removed");
    }

    #[test]
    fn agent_question_no_longer_detected() {
        let output = "Which file should I modify?";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "none", "agent_question pattern was removed");
    }

    #[test]
    fn detects_network_error() {
        let output = "Error: connection refused to localhost:5432";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "network_error");
    }

    #[test]
    fn detects_none() {
        let output = "Building project... done.";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "none");
        assert!(info.label.is_empty());
    }

    // --- detect_prompt edge cases ---

    #[test]
    fn detect_prompt_empty_string() {
        let info = detect_prompt("");
        assert_eq!(info.kind, "none");
    }

    #[test]
    fn detect_prompt_whitespace_only() {
        let info = detect_prompt("   \n  \t  \n  ");
        assert_eq!(info.kind, "none");
    }

    #[test]
    fn detect_prompt_credentials_missing() {
        let output = "Error: API key not found. Please set OPENAI_API_KEY.";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "credentials_missing");
    }

    #[test]
    fn detect_prompt_credentials_expired() {
        let output = "Token expired. Please re-authenticate.";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "credentials_missing");
    }

    #[test]
    fn detect_prompt_credentials_invalid() {
        let output = "credentials invalid — check your config";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "credentials_missing");
    }

    #[test]
    fn detect_prompt_yes_no_variants() {
        assert_eq!(
            detect_prompt("Continue? [Yes/No]").label,
            "Yes/No confirmation"
        );
        assert_eq!(
            detect_prompt("Save changes? [Y/N]").label,
            "Yes/No confirmation"
        );
        assert_eq!(
            detect_prompt("Overwrite? continue? (y)").label,
            "Yes/No confirmation"
        );
    }

    #[test]
    fn detect_prompt_press_enter_to_confirm() {
        let info = detect_prompt("Review the changes and press enter to confirm or esc to cancel");
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Press enter to confirm");
    }

    #[test]
    fn detect_prompt_rate_limit_quota_exceeded() {
        let info = detect_prompt("Error: quota exceeded for this billing period");
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "rate_limit");
    }

    #[test]
    fn detect_prompt_network_timeout() {
        let info = detect_prompt("connection timeout while fetching");
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "network_error");
    }

    #[test]
    fn detect_prompt_network_etimedout() {
        let info = detect_prompt("Error: ETIMEDOUT");
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "network_error");
    }

    #[test]
    fn detect_prompt_network_econnrefused() {
        let info = detect_prompt("Error: ECONNREFUSED");
        assert_eq!(info.kind, "complex");
        assert_eq!(info.label, "network_error");
    }

    #[test]
    fn detect_prompt_snippet_from_last_n_lines() {
        let output = "line1\nline2\nline3\nline4\nline5\nline6\nAuthentication failed";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "complex");
        assert!(info.snippet.contains("Authentication failed"));
    }

    // --- handle_simple_prompt async tests ---

    #[tokio::test]
    async fn handle_simple_prompt_claude_accept() {
        let p = PromptInfo::simple("Claude Code permission acceptance", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_workspace_trust() {
        let p = PromptInfo::simple("Workspace trust prompt", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_directory_trust() {
        let p = PromptInfo::simple("Directory trust confirmation", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_codex_model_switch() {
        let p = PromptInfo::simple("Codex model switch prompt", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_cursor_auto_run() {
        let p = PromptInfo::simple("Cursor auto-run prompt", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_press_enter_confirm() {
        let p = PromptInfo::simple("Press enter to confirm", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_press_enter_continue() {
        let p = PromptInfo::simple("Press enter to continue", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_yes_no() {
        let p = PromptInfo::simple("Yes/No confirmation", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_unknown_label() {
        let p = PromptInfo::simple("Unknown weird prompt", "snippet".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(!handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_complex_returns_false() {
        let p = PromptInfo::complex("auth_failure", "error".to_string());
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(!handled);
    }

    #[tokio::test]
    async fn handle_simple_prompt_none_returns_false() {
        let p = PromptInfo::none();
        let handled = handle_simple_prompt("%99", &p).await;
        assert!(!handled);
    }

    // --- whitespace collapsing via static WHITESPACE_RE ---

    #[test]
    fn detect_prompt_collapses_whitespace() {
        // Multi-line whitespace between keywords should still match after collapsing
        let output = "Yes, I accept\n\n\n   Press   enter\tto   confirm";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Claude Code permission acceptance");
    }

    #[test]
    fn detect_prompt_collapses_tabs_and_newlines() {
        let output = "Do\tyou\ttrust\tthe\tcontents\nof this directory?";
        let info = detect_prompt(output);
        assert_eq!(info.kind, "simple");
        assert_eq!(info.label, "Directory trust confirmation");
    }
}
