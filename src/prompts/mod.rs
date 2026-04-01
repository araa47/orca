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
            // Avoid false positives from rate-limit recovery messages injected
            // by the orchestrator (e.g. "The rate limit has cleared").  Without
            // this guard the daemon re-detects "rate limit" in its own recovery
            // text and escalates again, creating a feedback loop.
            if cp.label == "rate_limit"
                && (collapsed.contains("cleared")
                    || collapsed.contains("resolved")
                    || collapsed.contains("lifted"))
            {
                continue;
            }
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
mod tests;
