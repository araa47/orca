//! Worker state persistence — JSON + file lock.

use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use crate::config;

/// Error returned when saving a worker that already exists and overwrite is not allowed.
#[derive(Debug)]
pub struct DuplicateWorkerError(pub String);

impl std::fmt::Display for DuplicateWorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DuplicateWorkerError {}

/// A single orca worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    pub name: String,
    pub backend: String,
    pub task: String,
    pub dir: String,
    pub workdir: String,
    pub base_branch: String,
    pub orchestrator: String,
    #[serde(default)]
    pub orchestrator_pane: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub reply_channel: String,
    #[serde(default)]
    pub reply_to: String,
    #[serde(default)]
    pub reply_thread: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub depth: u32,
    #[serde(default)]
    pub spawned_by: String,
    #[serde(default = "default_layout")]
    pub layout: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default = "default_started_at")]
    pub started_at: String,
    #[serde(default)]
    pub last_event_at: String,
    #[serde(default)]
    pub done_reported: bool,
    #[serde(default)]
    pub process_exited: bool,
}

fn default_layout() -> String {
    "window".to_string()
}

fn default_status() -> String {
    "running".to_string()
}

fn default_started_at() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ---------------------------------------------------------------------------
// File locking
// ---------------------------------------------------------------------------

struct StateLock {
    _file: File,
}

impl StateLock {
    fn acquire() -> std::io::Result<Self> {
        config::ensure_home()?;
        let lock_path = config::lock_file();
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)?;
        file.lock_exclusive()?;
        Ok(StateLock { _file: file })
    }
}

// Lock is released when `_file` is dropped (closing the fd releases the flock).

// ---------------------------------------------------------------------------
// Raw JSON I/O
// ---------------------------------------------------------------------------

fn load_raw() -> HashMap<String, serde_json::Value> {
    let _ = config::ensure_home();
    let path = config::state_file();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };
    let text = text.trim();
    if text.is_empty() {
        return HashMap::new();
    }
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => map
            .into_iter()
            .filter_map(|(k, v)| if v.is_object() { Some((k, v)) } else { None })
            .collect(),
        Ok(_) => HashMap::new(),
        Err(_) => {
            let ts = Utc::now().format("%Y%m%d%H%M%S");
            let bak_name = format!("state.bak.{}", ts);
            let bak = path.with_file_name(bak_name);
            eprintln!(
                "WARNING: corrupt state file {}, preserving as {}",
                path.display(),
                bak.display()
            );
            let _ = fs::rename(&path, &bak);
            HashMap::new()
        }
    }
}

fn save_raw(data: &HashMap<String, serde_json::Value>) -> std::io::Result<()> {
    config::ensure_home()?;
    let state_path = config::state_file();
    let tmp = state_path.with_extension("tmp");
    let content = serde_json::to_string_pretty(data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        // Set permissions to 0644
        let perms = std::fs::Permissions::from_mode(0o644);
        file.set_permissions(perms)?;
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(content.as_bytes())?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }

    fs::rename(&tmp, &state_path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: parse a Worker from a JSON Value, ignoring unknown fields
// ---------------------------------------------------------------------------

fn value_to_worker(v: &serde_json::Value) -> Option<Worker> {
    serde_json::from_value::<Worker>(v.clone()).ok()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load all workers from the state file.
pub fn load_workers() -> HashMap<String, Worker> {
    let _lock = StateLock::acquire().ok();
    let raw = load_raw();
    let mut workers = HashMap::new();
    for (name, v) in &raw {
        if let Some(w) = value_to_worker(v) {
            workers.insert(name.clone(), w);
        }
    }
    workers
}

/// Persist a worker to the state file.
///
/// When `allow_overwrite` is `false`, returns `DuplicateWorkerError` if the
/// worker name already exists in the state file.
pub fn save_worker(
    worker: &Worker,
    allow_overwrite: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = StateLock::acquire()?;
    let mut raw = load_raw();
    if !allow_overwrite && raw.contains_key(&worker.name) {
        return Err(Box::new(DuplicateWorkerError(format!(
            "Worker '{}' already exists",
            worker.name
        ))));
    }
    let val = serde_json::to_value(worker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    raw.insert(worker.name.clone(), val);
    save_raw(&raw)?;
    Ok(())
}

/// Update only the status field of a worker. Returns the updated worker or `None`.
pub fn update_worker_status(name: &str, status: &str) -> std::io::Result<Option<Worker>> {
    let _lock = StateLock::acquire()?;
    let mut raw = load_raw();
    let entry = match raw.get_mut(name) {
        Some(v) => v,
        None => return Ok(None),
    };
    if let Some(obj) = entry.as_object_mut() {
        obj.insert(
            "status".to_string(),
            serde_json::Value::String(status.to_string()),
        );
    }
    save_raw(&raw)?;
    Ok(value_to_worker(&raw[name]))
}

/// Update arbitrary fields on a worker atomically. Returns updated Worker or `None`.
pub fn update_worker_fields(
    name: &str,
    updates: &HashMap<String, serde_json::Value>,
) -> std::io::Result<Option<Worker>> {
    let _lock = StateLock::acquire()?;
    let mut raw = load_raw();
    let entry = match raw.get_mut(name) {
        Some(v) => v,
        None => return Ok(None),
    };
    if let Some(obj) = entry.as_object_mut() {
        for (k, v) in updates {
            obj.insert(k.clone(), v.clone());
        }
    }
    save_raw(&raw)?;
    Ok(value_to_worker(&raw[name]))
}

/// Remove a worker from the state file.
pub fn remove_worker(name: &str) -> std::io::Result<()> {
    let _lock = StateLock::acquire()?;
    let mut raw = load_raw();
    raw.remove(name);
    save_raw(&raw)
}

/// Get a single worker by name.
pub fn get_worker(name: &str) -> Option<Worker> {
    load_workers().remove(name)
}

/// Return the set of all worker names.
pub fn worker_names() -> HashSet<String> {
    load_workers().into_keys().collect()
}

/// Count running workers spawned by a given orchestrator pane or session.
pub fn count_running_by_orchestrator(orchestrator_pane: &str, session_id: &str) -> usize {
    if orchestrator_pane.is_empty() && session_id.is_empty() {
        return 0;
    }
    load_workers()
        .values()
        .filter(|w| {
            if w.status != "running" {
                return false;
            }
            if !orchestrator_pane.is_empty() && w.orchestrator_pane == orchestrator_pane {
                return true;
            }
            if !session_id.is_empty() && w.session_id == session_id {
                return true;
            }
            false
        })
        .count()
}

/// True if this worker has any child workers (`spawned_by` = parent) still in play:
/// **`running`** or **`blocked`** (not yet done/dead/destroyed).
pub fn has_running_children(parent_name: &str) -> bool {
    load_workers()
        .values()
        .any(|w| w.spawned_by == parent_name && matches!(w.status.as_str(), "running" | "blocked"))
}

/// Remove workers with status `done`, `dead`, or `destroyed`. Returns removed names.
pub fn gc_workers() -> std::io::Result<Vec<String>> {
    let _lock = StateLock::acquire()?;
    let mut raw = load_raw();
    let to_remove: Vec<String> = raw
        .iter()
        .filter_map(|(name, v)| {
            let status = v.get("status")?.as_str()?;
            if matches!(status, "done" | "dead" | "destroyed") {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    let mut removed = Vec::new();
    for name in to_remove {
        raw.remove(&name);
        removed.push(name);
    }
    save_raw(&raw)?;
    Ok(removed)
}

#[cfg(test)]
mod tests;
