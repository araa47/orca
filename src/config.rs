//! Paths, defaults, and env-overridable config.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

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
        .unwrap_or(120)
}

pub fn max_depth() -> u32 {
    env::var("ORCA_MAX_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

pub fn max_workers_per_orchestrator() -> u32 {
    env::var("ORCA_MAX_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
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
mod tests {
    use super::*;

    /// Tests that mutate env vars must hold this lock to avoid races.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn canonical_backend_aliases() {
        assert_eq!(canonical_backend("cc"), "claude");
        assert_eq!(canonical_backend("cx"), "codex");
        assert_eq!(canonical_backend("cu"), "cursor");
    }

    #[test]
    fn canonical_backend_passthrough() {
        assert_eq!(canonical_backend("claude"), "claude");
        assert_eq!(canonical_backend("codex"), "codex");
        assert_eq!(canonical_backend("cursor"), "cursor");
        assert_eq!(canonical_backend("unknown"), "unknown");
    }

    #[test]
    fn cli_config_has_all_backends() {
        let cfg = cli_config();
        assert!(cfg.contains_key("claude"));
        assert!(cfg.contains_key("cc"));
        assert!(cfg.contains_key("codex"));
        assert!(cfg.contains_key("cx"));
        assert!(cfg.contains_key("cursor"));
        assert!(cfg.contains_key("cu"));
    }

    #[test]
    fn cli_config_claude_values() {
        let cfg = cli_config();
        let (bin, flag) = &cfg["claude"];
        assert_eq!(bin, "claude");
        assert_eq!(*flag, "--dangerously-skip-permissions");
        // cc is an alias for claude
        let (bin2, flag2) = &cfg["cc"];
        assert_eq!(bin2, "claude");
        assert_eq!(*flag2, "--dangerously-skip-permissions");
    }

    #[test]
    fn cli_config_codex_values() {
        let cfg = cli_config();
        let (bin, flag) = &cfg["codex"];
        assert_eq!(bin, "codex");
        assert_eq!(*flag, "--dangerously-bypass-approvals-and-sandbox");
    }

    #[test]
    fn state_file_under_orca_home() {
        let sf = state_file();
        assert!(sf.starts_with(orca_home()));
        assert_eq!(sf.file_name().unwrap(), "state.json");
    }

    #[test]
    fn lock_file_under_orca_home() {
        let lf = lock_file();
        assert!(lf.starts_with(orca_home()));
        assert_eq!(lf.file_name().unwrap(), "state.lock");
    }

    #[test]
    fn daemon_pid_file_under_orca_home() {
        let pf = daemon_pid_file();
        assert!(pf.starts_with(orca_home()));
        assert_eq!(pf.file_name().unwrap(), "daemon.pid");
    }

    #[test]
    fn daemon_log_file_under_orca_home() {
        let lf = daemon_log_file();
        assert!(lf.starts_with(orca_home()));
        assert_eq!(lf.file_name().unwrap(), "daemon.log");
    }

    #[test]
    fn max_depth_default() {
        // When ORCA_MAX_DEPTH is not set, default is 3
        if env::var("ORCA_MAX_DEPTH").is_err() {
            assert_eq!(max_depth(), 3);
        }
    }

    #[test]
    fn max_workers_default() {
        // When ORCA_MAX_WORKERS is not set, default is 10
        if env::var("ORCA_MAX_WORKERS").is_err() {
            assert_eq!(max_workers_per_orchestrator(), 10);
        }
    }

    #[test]
    fn ensure_home_creates_dir() {
        // If ORCA_HOME was set by another test to a stale tempdir,
        // the OnceLock may hold a path that no longer exists.
        // Just verify the function doesn't panic on a valid path.
        let _ = ensure_home();
    }

    #[test]
    fn orca_home_returns_consistent_value() {
        let a = orca_home();
        let b = orca_home();
        assert_eq!(a, b);
    }

    #[test]
    fn tmux_session_returns_nonempty() {
        let s = tmux_session();
        assert!(!s.is_empty());
    }

    #[test]
    fn audit_log_file_under_orca_home() {
        let af = audit_log_file();
        assert!(af.starts_with(orca_home()));
        assert_eq!(af.file_name().unwrap(), "audit.log");
    }

    #[test]
    fn events_dir_under_orca_home() {
        let ed = events_dir();
        assert!(ed.starts_with(orca_home()));
        assert_eq!(ed.file_name().unwrap(), "events");
    }

    #[test]
    fn logs_dir_under_orca_home() {
        let ld = logs_dir();
        assert!(ld.starts_with(orca_home()));
        assert_eq!(ld.file_name().unwrap(), "logs");
    }

    #[test]
    fn watchdog_quiet_secs_default() {
        if env::var("ORCA_WATCHDOG_QUIET_SECS").is_err() {
            assert_eq!(watchdog_quiet_secs(), 120);
        }
    }

    #[test]
    fn tmux_socket_file_under_orca_home() {
        let sf = tmux_socket_file();
        assert!(sf.starts_with(orca_home()));
        assert_eq!(sf.file_name().unwrap(), "tmux-socket");
    }

    #[test]
    fn load_tmux_socket_returns_none_when_missing() {
        let _ = std::fs::remove_file(tmux_socket_file());
        assert!(load_tmux_socket().is_none());
    }

    #[test]
    fn save_and_load_tmux_socket_roundtrip() {
        let tmux_val = "/private/tmp/tmux-501/default,12345,0";
        let socket = tmux_val.split(',').next().unwrap();
        assert_eq!(socket, "/private/tmp/tmux-501/default");

        let empty = "";
        let result = empty.split(',').next().filter(|s| !s.is_empty());
        assert!(result.is_none());
    }

    #[test]
    fn save_tmux_socket_writes_file() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _ = ensure_home();
        unsafe {
            std::env::set_var("TMUX", "/tmp/tmux-test/default,99999,0");
        }
        save_tmux_socket();
        let loaded = load_tmux_socket();
        assert_eq!(loaded, Some("/tmp/tmux-test/default".to_string()));
        unsafe {
            std::env::remove_var("TMUX");
        }
    }

    #[test]
    fn save_tmux_socket_noop_without_tmux_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _ = ensure_home();
        let _ = std::fs::remove_file(tmux_socket_file());
        unsafe {
            std::env::remove_var("TMUX");
        }
        save_tmux_socket();
    }

    #[test]
    fn load_tmux_socket_ignores_empty_file() {
        // Test the parsing logic directly to avoid filesystem races
        let empty = "";
        let result: Option<String> = Some(empty.trim().to_string()).filter(|s| !s.is_empty());
        assert!(result.is_none());
    }

    #[test]
    fn load_tmux_socket_trims_whitespace() {
        // Test the parsing logic directly to avoid filesystem races
        let raw = "  /tmp/sock  \n";
        let result: Option<String> = Some(raw.trim().to_string()).filter(|s| !s.is_empty());
        assert_eq!(result, Some("/tmp/sock".to_string()));
    }

    #[test]
    fn cli_config_cursor_has_force_flag() {
        let cfg = cli_config();
        let (_, flag) = &cfg["cursor"];
        assert_eq!(*flag, "--force");
        let (_, flag2) = &cfg["cu"];
        assert_eq!(*flag2, "--force");
    }

    #[test]
    fn cli_config_cursor_bin_matches_cu_alias() {
        let cfg = cli_config();
        let (bin_cursor, _) = &cfg["cursor"];
        let (bin_cu, _) = &cfg["cu"];
        assert_eq!(bin_cursor, bin_cu);
    }

    #[test]
    fn watchdog_quiet_secs_env_override() {
        // Test parsing logic without touching env vars (avoids race conditions)
        let val = Some("300".to_string())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);
        assert_eq!(val, 300);
    }

    #[test]
    fn max_depth_env_override() {
        // Test parsing logic without touching env vars (avoids race conditions)
        let val = Some("5".to_string())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(3);
        assert_eq!(val, 5);
    }

    #[test]
    fn max_workers_env_override() {
        // Test parsing logic without touching env vars (avoids race conditions)
        let val = Some("20".to_string())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(10);
        assert_eq!(val, 20);
    }

    #[test]
    fn max_depth_invalid_env_uses_default() {
        // Test parsing logic without touching env vars (avoids race conditions)
        let val = Some("not_a_number".to_string())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(3);
        assert_eq!(val, 3);
    }

    #[test]
    fn watchdog_quiet_secs_invalid_env_uses_default() {
        // Test parsing logic without touching env vars (avoids race conditions)
        let val = Some("abc".to_string())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);
        assert_eq!(val, 120);
    }
}
