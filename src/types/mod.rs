//! Shared enums for backends and worker statuses.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Agent backend: the CLI tool that powers a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Claude,
    Codex,
    Cursor,
    Openclaw,
}

impl Backend {
    /// Two-letter shorthand used in CLI aliases.
    #[allow(dead_code)]
    pub fn short(&self) -> &'static str {
        match self {
            Backend::Claude => "cc",
            Backend::Codex => "cx",
            Backend::Cursor => "cu",
            Backend::Openclaw => "oc",
        }
    }

    /// True for backends that correspond to real worker agents (not orchestrator-only).
    #[allow(dead_code)]
    pub fn is_worker_backend(&self) -> bool {
        !matches!(self, Backend::Openclaw)
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Backend::Claude => "claude",
            Backend::Codex => "codex",
            Backend::Cursor => "cursor",
            Backend::Openclaw => "openclaw",
        };
        f.write_str(s)
    }
}

/// Error returned when parsing a string that does not match any known backend.
#[derive(Debug, Clone)]
pub struct ParseBackendError(pub String);

impl fmt::Display for ParseBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown backend: '{}'", self.0)
    }
}

impl std::error::Error for ParseBackendError {}

impl std::str::FromStr for Backend {
    type Err = ParseBackendError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cc" | "claude" => Ok(Backend::Claude),
            "cx" | "codex" => Ok(Backend::Codex),
            "cu" | "cursor" => Ok(Backend::Cursor),
            "oc" | "openclaw" => Ok(Backend::Openclaw),
            other => Err(ParseBackendError(other.to_string())),
        }
    }
}

impl Serialize for Backend {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Backend {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<Backend>().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// Who receives completion / stuck notifications for a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Orchestrator {
    Backend(Backend),
    Custom(String),
    None,
}

impl Orchestrator {
    /// Return the inner backend, if any.
    #[allow(dead_code)]
    pub fn as_backend(&self) -> Option<&Backend> {
        match self {
            Orchestrator::Backend(b) => Some(b),
            Orchestrator::Custom(_) | Orchestrator::None => None,
        }
    }

    /// True if this is a custom (user-defined gateway) orchestrator.
    #[allow(dead_code)]
    pub fn is_custom(&self) -> bool {
        matches!(self, Orchestrator::Custom(_))
    }

    /// Return the custom gateway name, if any.
    #[allow(dead_code)]
    pub fn custom_name(&self) -> Option<&str> {
        match self {
            Orchestrator::Custom(name) => Some(name),
            _ => None,
        }
    }
}

impl fmt::Display for Orchestrator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Orchestrator::Backend(b) => b.fmt(f),
            Orchestrator::Custom(name) => f.write_str(name),
            Orchestrator::None => f.write_str("none"),
        }
    }
}

/// Error returned when parsing a string that does not match any known orchestrator.
#[derive(Debug, Clone)]
pub struct ParseOrchestratorError(pub String);

impl fmt::Display for ParseOrchestratorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown orchestrator: '{}'", self.0)
    }
}

impl std::error::Error for ParseOrchestratorError {}

impl std::str::FromStr for Orchestrator {
    type Err = ParseOrchestratorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(ParseOrchestratorError(s.to_string()));
        }
        if s == "none" {
            return Ok(Orchestrator::None);
        }
        match s.parse::<Backend>() {
            Ok(b) => Ok(Orchestrator::Backend(b)),
            Err(_) => Ok(Orchestrator::Custom(s.to_string())),
        }
    }
}

impl Serialize for Orchestrator {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Orchestrator {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<Orchestrator>().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// WorkerStatus
// ---------------------------------------------------------------------------

/// Lifecycle status of a worker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WorkerStatus {
    #[default]
    Running,
    Blocked,
    Done,
    Dead,
    Destroyed,
}

impl WorkerStatus {
    /// True for statuses where the worker is still in play.
    pub fn is_active(&self) -> bool {
        matches!(self, WorkerStatus::Running | WorkerStatus::Blocked)
    }

    /// True for terminal statuses (worker is finished).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkerStatus::Done | WorkerStatus::Dead | WorkerStatus::Destroyed
        )
    }

    /// Unicode symbol for display in `orca list`.
    pub fn symbol(&self) -> &'static str {
        match self {
            WorkerStatus::Running => "\u{25b6}",    // ▶
            WorkerStatus::Blocked => "\u{23f8}",    // ⏸
            WorkerStatus::Done => "\u{2713}",       // ✓
            WorkerStatus::Dead => "\u{2717}",       // ✗
            WorkerStatus::Destroyed => "\u{1f480}", // 💀
        }
    }
}

impl fmt::Display for WorkerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            WorkerStatus::Running => "running",
            WorkerStatus::Blocked => "blocked",
            WorkerStatus::Done => "done",
            WorkerStatus::Dead => "dead",
            WorkerStatus::Destroyed => "destroyed",
        };
        f.write_str(s)
    }
}

/// Error returned when parsing a string that does not match any known status.
#[derive(Debug, Clone)]
pub struct ParseWorkerStatusError(pub String);

impl fmt::Display for ParseWorkerStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown worker status: '{}'", self.0)
    }
}

impl std::error::Error for ParseWorkerStatusError {}

impl std::str::FromStr for WorkerStatus {
    type Err = ParseWorkerStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(WorkerStatus::Running),
            "blocked" => Ok(WorkerStatus::Blocked),
            "done" => Ok(WorkerStatus::Done),
            "dead" => Ok(WorkerStatus::Dead),
            "destroyed" => Ok(WorkerStatus::Destroyed),
            other => Err(ParseWorkerStatusError(other.to_string())),
        }
    }
}

impl Serialize for WorkerStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for WorkerStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<WorkerStatus>().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Backend ---

    #[test]
    fn backend_display() {
        assert_eq!(Backend::Claude.to_string(), "claude");
        assert_eq!(Backend::Codex.to_string(), "codex");
        assert_eq!(Backend::Cursor.to_string(), "cursor");
        assert_eq!(Backend::Openclaw.to_string(), "openclaw");
    }

    #[test]
    fn backend_short() {
        assert_eq!(Backend::Claude.short(), "cc");
        assert_eq!(Backend::Codex.short(), "cx");
        assert_eq!(Backend::Cursor.short(), "cu");
        assert_eq!(Backend::Openclaw.short(), "oc");
    }

    #[test]
    fn backend_from_str_canonical() {
        assert_eq!("claude".parse::<Backend>().unwrap(), Backend::Claude);
        assert_eq!("codex".parse::<Backend>().unwrap(), Backend::Codex);
        assert_eq!("cursor".parse::<Backend>().unwrap(), Backend::Cursor);
        assert_eq!("openclaw".parse::<Backend>().unwrap(), Backend::Openclaw);
    }

    #[test]
    fn backend_from_str_alias() {
        assert_eq!("cc".parse::<Backend>().unwrap(), Backend::Claude);
        assert_eq!("cx".parse::<Backend>().unwrap(), Backend::Codex);
        assert_eq!("cu".parse::<Backend>().unwrap(), Backend::Cursor);
        assert_eq!("oc".parse::<Backend>().unwrap(), Backend::Openclaw);
    }

    #[test]
    fn backend_from_str_error() {
        assert!("unknown".parse::<Backend>().is_err());
        assert!("".parse::<Backend>().is_err());
    }

    #[test]
    fn backend_is_worker_backend() {
        assert!(Backend::Claude.is_worker_backend());
        assert!(Backend::Codex.is_worker_backend());
        assert!(Backend::Cursor.is_worker_backend());
        assert!(!Backend::Openclaw.is_worker_backend());
    }

    #[test]
    fn backend_serde_roundtrip() {
        for b in [
            Backend::Claude,
            Backend::Codex,
            Backend::Cursor,
            Backend::Openclaw,
        ] {
            let json = serde_json::to_string(&b).unwrap();
            let parsed: Backend = serde_json::from_str(&json).unwrap();
            assert_eq!(b, parsed);
        }
    }

    #[test]
    fn backend_deserialize_alias() {
        let b: Backend = serde_json::from_str("\"cc\"").unwrap();
        assert_eq!(b, Backend::Claude);
    }

    // --- Orchestrator ---

    #[test]
    fn orchestrator_display() {
        assert_eq!(Orchestrator::None.to_string(), "none");
        assert_eq!(Orchestrator::Backend(Backend::Claude).to_string(), "claude");
        assert_eq!(Orchestrator::Custom("hermes".into()).to_string(), "hermes");
    }

    #[test]
    fn orchestrator_from_str() {
        assert_eq!("none".parse::<Orchestrator>().unwrap(), Orchestrator::None);
        assert_eq!(
            "claude".parse::<Orchestrator>().unwrap(),
            Orchestrator::Backend(Backend::Claude)
        );
        assert_eq!(
            "cc".parse::<Orchestrator>().unwrap(),
            Orchestrator::Backend(Backend::Claude)
        );
        assert_eq!(
            "openclaw".parse::<Orchestrator>().unwrap(),
            Orchestrator::Backend(Backend::Openclaw)
        );
        // Unknown names become Custom
        assert_eq!(
            "hermes".parse::<Orchestrator>().unwrap(),
            Orchestrator::Custom("hermes".into())
        );
        assert_eq!(
            "mybot".parse::<Orchestrator>().unwrap(),
            Orchestrator::Custom("mybot".into())
        );
        // Empty string is an error
        assert!("".parse::<Orchestrator>().is_err());
    }

    #[test]
    fn orchestrator_as_backend() {
        assert_eq!(
            Orchestrator::Backend(Backend::Codex).as_backend(),
            Some(&Backend::Codex)
        );
        assert_eq!(Orchestrator::None.as_backend(), None);
        assert_eq!(Orchestrator::Custom("hermes".into()).as_backend(), None);
    }

    #[test]
    fn orchestrator_is_custom() {
        assert!(Orchestrator::Custom("hermes".into()).is_custom());
        assert!(!Orchestrator::None.is_custom());
        assert!(!Orchestrator::Backend(Backend::Claude).is_custom());
    }

    #[test]
    fn orchestrator_custom_name() {
        assert_eq!(
            Orchestrator::Custom("hermes".into()).custom_name(),
            Some("hermes")
        );
        assert_eq!(Orchestrator::None.custom_name(), None);
        assert_eq!(Orchestrator::Backend(Backend::Claude).custom_name(), None);
    }

    #[test]
    fn orchestrator_serde_roundtrip() {
        for o in [
            Orchestrator::None,
            Orchestrator::Backend(Backend::Claude),
            Orchestrator::Backend(Backend::Openclaw),
            Orchestrator::Custom("hermes".into()),
        ] {
            let json = serde_json::to_string(&o).unwrap();
            let parsed: Orchestrator = serde_json::from_str(&json).unwrap();
            assert_eq!(o, parsed);
        }
    }

    #[test]
    fn orchestrator_deserialize_alias() {
        let o: Orchestrator = serde_json::from_str("\"cx\"").unwrap();
        assert_eq!(o, Orchestrator::Backend(Backend::Codex));
    }

    #[test]
    fn orchestrator_deserialize_custom() {
        let o: Orchestrator = serde_json::from_str("\"hermes\"").unwrap();
        assert_eq!(o, Orchestrator::Custom("hermes".into()));
    }

    // --- WorkerStatus ---

    #[test]
    fn worker_status_display() {
        assert_eq!(WorkerStatus::Running.to_string(), "running");
        assert_eq!(WorkerStatus::Blocked.to_string(), "blocked");
        assert_eq!(WorkerStatus::Done.to_string(), "done");
        assert_eq!(WorkerStatus::Dead.to_string(), "dead");
        assert_eq!(WorkerStatus::Destroyed.to_string(), "destroyed");
    }

    #[test]
    fn worker_status_from_str() {
        assert_eq!(
            "running".parse::<WorkerStatus>().unwrap(),
            WorkerStatus::Running
        );
        assert_eq!(
            "blocked".parse::<WorkerStatus>().unwrap(),
            WorkerStatus::Blocked
        );
        assert_eq!("done".parse::<WorkerStatus>().unwrap(), WorkerStatus::Done);
        assert_eq!("dead".parse::<WorkerStatus>().unwrap(), WorkerStatus::Dead);
        assert_eq!(
            "destroyed".parse::<WorkerStatus>().unwrap(),
            WorkerStatus::Destroyed
        );
        assert!("unknown".parse::<WorkerStatus>().is_err());
    }

    #[test]
    fn worker_status_default() {
        assert_eq!(WorkerStatus::default(), WorkerStatus::Running);
    }

    #[test]
    fn worker_status_is_active() {
        assert!(WorkerStatus::Running.is_active());
        assert!(WorkerStatus::Blocked.is_active());
        assert!(!WorkerStatus::Done.is_active());
        assert!(!WorkerStatus::Dead.is_active());
        assert!(!WorkerStatus::Destroyed.is_active());
    }

    #[test]
    fn worker_status_is_terminal() {
        assert!(!WorkerStatus::Running.is_terminal());
        assert!(!WorkerStatus::Blocked.is_terminal());
        assert!(WorkerStatus::Done.is_terminal());
        assert!(WorkerStatus::Dead.is_terminal());
        assert!(WorkerStatus::Destroyed.is_terminal());
    }

    #[test]
    fn worker_status_symbol() {
        assert_eq!(WorkerStatus::Running.symbol(), "\u{25b6}");
        assert_eq!(WorkerStatus::Blocked.symbol(), "\u{23f8}");
        assert_eq!(WorkerStatus::Done.symbol(), "\u{2713}");
        assert_eq!(WorkerStatus::Dead.symbol(), "\u{2717}");
        assert_eq!(WorkerStatus::Destroyed.symbol(), "\u{1f480}");
    }

    #[test]
    fn worker_status_serde_roundtrip() {
        for s in [
            WorkerStatus::Running,
            WorkerStatus::Blocked,
            WorkerStatus::Done,
            WorkerStatus::Dead,
            WorkerStatus::Destroyed,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let parsed: WorkerStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, parsed);
        }
    }

    #[test]
    fn worker_status_deserialize_from_json_string() {
        let s: WorkerStatus = serde_json::from_str("\"running\"").unwrap();
        assert_eq!(s, WorkerStatus::Running);
    }

    // --- Backwards compatibility ---

    #[test]
    fn existing_json_still_deserializes() {
        // Simulate an existing state.json worker entry with string fields
        let json = serde_json::json!({
            "backend": "claude",
            "orchestrator": "cc",
            "status": "running"
        });

        #[derive(Deserialize)]
        struct Mini {
            backend: Backend,
            orchestrator: Orchestrator,
            status: WorkerStatus,
        }

        let m: Mini = serde_json::from_value(json).unwrap();
        assert_eq!(m.backend, Backend::Claude);
        assert_eq!(m.orchestrator, Orchestrator::Backend(Backend::Claude));
        assert_eq!(m.status, WorkerStatus::Running);
    }
}
