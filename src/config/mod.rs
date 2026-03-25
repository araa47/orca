//! Paths, defaults, and env-overridable config.
//!
//! Settings are resolved in order: **env var > config.yaml > hardcoded default**.

use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// YAML config structs
// ---------------------------------------------------------------------------

/// A user-defined gateway for custom orchestrators.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GatewayConfig {
    /// Shell command template for wake/stuck/warn notifications.
    /// Placeholders: `{text}`, `{worker}`, `{channel}`, `{target}`, `{thread}`
    pub wake: Option<String>,
    /// Shell command template for reply-routed messages (when reply_channel is set).
    /// Placeholders: `{text}`, `{worker}`, `{channel}`, `{target}`, `{thread}`
    pub reply: Option<String>,
}

impl GatewayConfig {
    /// True if this gateway defines a reply template (meaning --reply-channel is required).
    pub fn requires_reply_channel(&self) -> bool {
        self.reply.is_some()
    }
}

/// Top-level YAML config (`$ORCA_HOME/config.yaml`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OrcaConfig {
    pub watchdog_quiet_secs: Option<u64>,
    pub max_depth: Option<u32>,
    pub max_workers: Option<u32>,
    #[serde(default)]
    pub gateways: HashMap<String, GatewayConfig>,
}

/// Load and cache the YAML config file.
pub fn load_config() -> &'static OrcaConfig {
    static CONFIG: OnceLock<OrcaConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let path = orca_home().join("config.yaml");
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_yaml::from_str(&contents).unwrap_or_default(),
            Err(_) => OrcaConfig::default(),
        }
    })
}

/// Look up a custom gateway by name.
pub fn gateway(name: &str) -> Option<&'static GatewayConfig> {
    load_config().gateways.get(name)
}

/// Return all configured gateway names.
pub fn gateway_names() -> Vec<&'static str> {
    load_config().gateways.keys().map(|s| s.as_str()).collect()
}

/// Return the orca home directory (`$ORCA_HOME` or `~/.orca`).
pub fn orca_home() -> &'static PathBuf {
    static ORCA_HOME: OnceLock<PathBuf> = OnceLock::new();
    ORCA_HOME.get_or_init(|| {
        if let Ok(val) = env::var("ORCA_HOME") {
            PathBuf::from(val)
        } else {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".orca")
        }
    })
}

pub fn state_file() -> PathBuf {
    orca_home().join("state.json")
}

pub fn lock_file() -> PathBuf {
    orca_home().join("state.lock")
}

pub fn daemon_pid_file() -> PathBuf {
    orca_home().join("daemon.pid")
}

pub fn daemon_log_file() -> PathBuf {
    orca_home().join("daemon.log")
}

pub fn audit_log_file() -> PathBuf {
    orca_home().join("audit.log")
}

pub fn events_dir() -> PathBuf {
    orca_home().join("events")
}

pub fn logs_dir() -> PathBuf {
    orca_home().join("logs")
}

pub fn tmux_socket_file() -> PathBuf {
    orca_home().join("tmux-socket")
}

/// Persist the tmux socket path so the daemon can reconnect after manual restart.
///
/// Extracts the socket from `$TMUX` (format: `/path/to/socket,pid,idx`).
pub fn save_tmux_socket() {
    if let Ok(tmux_val) = env::var("TMUX")
        && let Some(socket) = tmux_val.split(',').next()
        && !socket.is_empty()
    {
        let _ = std::fs::write(tmux_socket_file(), socket);
    }
}

/// Load the saved tmux socket path, if any.
pub fn load_tmux_socket() -> Option<String> {
    std::fs::read_to_string(tmux_socket_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn watchdog_quiet_secs() -> u64 {
    env::var("ORCA_WATCHDOG_QUIET_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .or(load_config().watchdog_quiet_secs)
        .unwrap_or(120)
}

pub fn max_depth() -> u32 {
    env::var("ORCA_MAX_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .or(load_config().max_depth)
        .unwrap_or(3)
}

pub fn max_workers_per_orchestrator() -> u32 {
    env::var("ORCA_MAX_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .or(load_config().max_workers)
        .unwrap_or(10)
}

/// Detect the current tmux session name.
///
/// Checks `$ORCA_TMUX_SESSION` first, then queries tmux if `$TMUX` is set,
/// and falls back to `"main"`.
pub fn tmux_session() -> &'static str {
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION
        .get_or_init(|| {
            if let Ok(name) = env::var("ORCA_TMUX_SESSION")
                && !name.is_empty()
            {
                return name;
            }
            if env::var("TMUX").is_ok()
                && let Ok(output) = Command::new("tmux")
                    .args(["display-message", "-p", "#{session_name}"])
                    .output()
                && output.status.success()
            {
                let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !name.is_empty() {
                    return name;
                }
            }
            "main".to_string()
        })
        .as_str()
}

/// Detect the cursor agent binary — `"agent"` on Linux, `"cursor agent"` on macOS.
fn cursor_bin() -> String {
    if which::which("agent").is_ok() {
        return "agent".to_string();
    }
    if which::which("cursor").is_ok() {
        return "cursor agent".to_string();
    }
    "agent".to_string()
}

/// CLI backend configuration: maps backend name -> (binary, yolo_flag).
pub fn cli_config() -> &'static HashMap<&'static str, (String, &'static str)> {
    static CONFIG: OnceLock<HashMap<&str, (String, &str)>> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let cursor = cursor_bin();
        let mut m = HashMap::new();
        m.insert(
            "claude",
            ("claude".to_string(), "--dangerously-skip-permissions"),
        );
        m.insert(
            "cc",
            ("claude".to_string(), "--dangerously-skip-permissions"),
        );
        m.insert(
            "codex",
            (
                "codex".to_string(),
                "--dangerously-bypass-approvals-and-sandbox",
            ),
        );
        m.insert(
            "cx",
            (
                "codex".to_string(),
                "--dangerously-bypass-approvals-and-sandbox",
            ),
        );
        m.insert("cursor", (cursor.clone(), "--force"));
        m.insert("cu", (cursor, "--force"));
        m
    })
}

/// Map short aliases to canonical backend names.
pub fn canonical_backend(name: &str) -> &str {
    static CANONICAL: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    let map = CANONICAL.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("cc", "claude");
        m.insert("cx", "codex");
        m.insert("cu", "cursor");
        m
    });
    map.get(name).copied().unwrap_or(name)
}

/// Create the orca home directory (and events/logs subdirs) if they don't exist.
pub fn ensure_home() -> std::io::Result<()> {
    std::fs::create_dir_all(orca_home())?;
    std::fs::create_dir_all(events_dir())?;
    std::fs::create_dir_all(logs_dir())?;
    Ok(())
}

/// Append a timestamped line to `audit.log`. Usable from any module.
pub fn audit(msg: &str) {
    let _ = ensure_home();
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] {msg}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_log_file())
    {
        let _ = std::io::Write::write_all(&mut f, line.as_bytes());
    }
}

#[cfg(test)]
mod tests;
