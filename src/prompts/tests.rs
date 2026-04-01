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
fn rate_limit_cleared_is_not_blocker() {
    let output = "Continue with your task. The rate limit has cleared. \
                  If you already opened a PR, you're done.";
    let info = detect_prompt(output);
    assert_eq!(
        info.kind, "none",
        "recovery message containing 'rate limit' + 'cleared' should not trigger"
    );
}

#[test]
fn rate_limit_resolved_is_not_blocker() {
    let output = "The rate limit has been resolved. Continue working.";
    let info = detect_prompt(output);
    assert_eq!(info.kind, "none");
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
