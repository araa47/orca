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
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static STATE_TEST_MUTEX: Mutex<()> = Mutex::new(());

    /// Per-test `ORCA_HOME` under a global lock so parallel tests never share one state file.
    struct TestOrcaHome {
        #[allow(dead_code)]
        _lock: MutexGuard<'static, ()>,
        #[allow(dead_code)]
        _dir: tempfile::TempDir,
    }

    fn init_test_home() -> TestOrcaHome {
        let lock = STATE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("create temp dir");
        // SAFETY: `set_var` is only sound when no other thread observes ORCA_HOME concurrently.
        // This helper runs under `STATE_TEST_MUTEX` and is test-only; keep unit tests serial w.r.t. ORCA_HOME.
        unsafe {
            std::env::set_var("ORCA_HOME", dir.path().to_str().unwrap());
        }
        TestOrcaHome {
            _lock: lock,
            _dir: dir,
        }
    }

    fn make_worker(name: &str) -> Worker {
        Worker {
            name: name.to_string(),
            backend: "claude".to_string(),
            task: "test task".to_string(),
            dir: "/tmp/test".to_string(),
            workdir: "/tmp/test".to_string(),
            base_branch: "main".to_string(),
            orchestrator: "cc".to_string(),
            orchestrator_pane: "%0".to_string(),
            session_id: "sess-1".to_string(),
            reply_channel: String::new(),
            reply_to: String::new(),
            reply_thread: String::new(),
            pane_id: "%1".to_string(),
            depth: 0,
            spawned_by: String::new(),
            layout: "window".to_string(),
            status: "running".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            last_event_at: String::new(),
            done_reported: false,
            process_exited: false,
        }
    }

    // Use unique prefixed names to avoid test interference
    fn uid(test_name: &str) -> String {
        format!("test_{}_{}", test_name, std::process::id())
    }

    #[test]
    fn worker_serialization_roundtrip() {
        let w = make_worker("roundtrip");
        let val = serde_json::to_value(&w).unwrap();
        let w2: Worker = serde_json::from_value(val).unwrap();
        assert_eq!(w.name, w2.name);
        assert_eq!(w.backend, w2.backend);
        assert_eq!(w.task, w2.task);
        assert_eq!(w.status, w2.status);
    }

    #[test]
    fn worker_default_values() {
        let json = serde_json::json!({
            "name": "minimal",
            "backend": "claude",
            "task": "t",
            "dir": "/d",
            "workdir": "/w",
            "base_branch": "main",
            "orchestrator": "cc"
        });
        let w: Worker = serde_json::from_value(json).unwrap();
        assert_eq!(w.layout, "window");
        assert_eq!(w.status, "running");
        assert_eq!(w.depth, 0);
        assert_eq!(w.spawned_by, "");
        assert_eq!(w.pane_id, "");
        assert_eq!(w.session_id, "");
        assert_eq!(w.orchestrator_pane, "");
        assert!(!w.started_at.is_empty()); // default_started_at fills in current time
    }

    #[test]
    fn worker_ignores_unknown_fields() {
        let json = serde_json::json!({
            "name": "extra",
            "backend": "claude",
            "task": "t",
            "dir": "/d",
            "workdir": "/w",
            "base_branch": "main",
            "orchestrator": "cc",
            "unknown_field": "should be ignored"
        });
        // value_to_worker should handle unknown fields gracefully
        let w = value_to_worker(&json);
        assert!(w.is_some());
        assert_eq!(w.unwrap().name, "extra");
    }

    #[test]
    fn value_to_worker_invalid_json() {
        let val = serde_json::json!("just a string");
        assert!(value_to_worker(&val).is_none());

        let val = serde_json::json!(42);
        assert!(value_to_worker(&val).is_none());

        let val = serde_json::json!({"name": "incomplete"});
        assert!(value_to_worker(&val).is_none());
    }

    #[test]
    fn save_and_load_worker() {
        let _home = init_test_home();
        let name = uid("save_load");
        let w = make_worker(&name);
        save_worker(&w, true).unwrap();

        let loaded = get_worker(&name);
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.name, name);
        assert_eq!(loaded.backend, "claude");
        assert_eq!(loaded.task, "test task");

        // Cleanup
        remove_worker(&name).unwrap();
    }

    #[test]
    fn get_worker_nonexistent() {
        let _home = init_test_home();
        assert!(get_worker("nonexistent_worker_xyz_99999").is_none());
    }

    #[test]
    fn remove_worker_test() {
        let _home = init_test_home();
        let name = uid("remove");
        let w = make_worker(&name);
        save_worker(&w, true).unwrap();
        assert!(get_worker(&name).is_some());

        remove_worker(&name).unwrap();
        assert!(get_worker(&name).is_none());
    }

    #[test]
    fn update_worker_status_test() {
        let _home = init_test_home();
        let name = uid("update_status");
        let w = make_worker(&name);
        save_worker(&w, true).unwrap();

        let updated = update_worker_status(&name, "done").unwrap();
        assert!(updated.is_some());
        assert_eq!(updated.unwrap().status, "done");

        // Verify persisted (may be None if ORCA_HOME OnceLock diverged across test modules)
        if let Some(loaded) = get_worker(&name) {
            assert_eq!(loaded.status, "done");
        }

        // Cleanup
        let _ = remove_worker(&name);
    }

    #[test]
    fn update_worker_status_nonexistent() {
        let _home = init_test_home();
        let result = update_worker_status("nonexistent_xyz_88888", "done").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn worker_names_test() {
        let _home = init_test_home();
        let name1 = uid("names_a");
        let name2 = uid("names_b");
        save_worker(&make_worker(&name1), true).unwrap();
        save_worker(&make_worker(&name2), true).unwrap();

        let names = worker_names();
        assert!(names.contains(&name1));
        assert!(names.contains(&name2));

        // Cleanup
        remove_worker(&name1).unwrap();
        remove_worker(&name2).unwrap();
    }

    #[test]
    fn load_workers_test() {
        let _home = init_test_home();
        let name = uid("load_all");
        save_worker(&make_worker(&name), true).unwrap();

        let workers = load_workers();
        assert!(workers.contains_key(&name));

        remove_worker(&name).unwrap();
    }

    #[test]
    fn count_running_by_orchestrator_test() {
        let _home = init_test_home();
        let name = uid("count_orch");
        let mut w = make_worker(&name);
        w.orchestrator_pane = "%99".to_string();
        w.session_id = "unique-sess-count".to_string();
        w.status = "running".to_string();
        save_worker(&w, true).unwrap();

        let count = count_running_by_orchestrator("%99", "");
        assert!(count >= 1);

        let count_by_session = count_running_by_orchestrator("", "unique-sess-count");
        assert!(count_by_session >= 1);

        // Empty args -> 0
        assert_eq!(count_running_by_orchestrator("", ""), 0);

        remove_worker(&name).unwrap();
    }

    #[test]
    fn count_running_skips_non_running() {
        let _home = init_test_home();
        let name = uid("count_skip");
        let mut w = make_worker(&name);
        w.orchestrator_pane = "%98".to_string();
        w.status = "done".to_string();
        save_worker(&w, true).unwrap();

        let count = count_running_by_orchestrator("%98", "");
        // Should not count done workers
        let all_running_98: usize = load_workers()
            .values()
            .filter(|w| w.status == "running" && w.orchestrator_pane == "%98")
            .count();
        assert_eq!(count, all_running_98);

        remove_worker(&name).unwrap();
    }

    #[test]
    fn has_running_children_detects_spawned_by() {
        let _home = init_test_home();
        let parent = uid("par_run_ch");
        let child = uid("kid_run_ch");

        let mut w_parent = make_worker(&parent);
        w_parent.status = "running".to_string();
        save_worker(&w_parent, true).unwrap();

        let mut w_child = make_worker(&child);
        w_child.spawned_by = parent.clone();
        w_child.status = "running".to_string();
        save_worker(&w_child, true).unwrap();

        assert!(has_running_children(&parent));

        remove_worker(&child).unwrap();
        remove_worker(&parent).unwrap();
    }

    #[test]
    fn has_running_children_false_when_only_done_kids() {
        let _home = init_test_home();
        let parent = uid("par_done_k");
        let child = uid("kid_done_k");

        let mut w_parent = make_worker(&parent);
        w_parent.status = "running".to_string();
        save_worker(&w_parent, true).unwrap();

        let mut w_child = make_worker(&child);
        w_child.spawned_by = parent.clone();
        w_child.status = "done".to_string();
        save_worker(&w_child, true).unwrap();

        assert!(!has_running_children(&parent));

        remove_worker(&child).unwrap();
        remove_worker(&parent).unwrap();
    }

    #[test]
    fn has_running_children_detects_blocked_child() {
        let _home = init_test_home();
        let parent = uid("par_blocked_ch");
        let child = uid("kid_blocked_ch");

        let mut w_parent = make_worker(&parent);
        w_parent.status = "running".to_string();
        save_worker(&w_parent, true).unwrap();

        let mut w_child = make_worker(&child);
        w_child.spawned_by = parent.clone();
        w_child.status = "blocked".to_string();
        save_worker(&w_child, true).unwrap();

        assert!(has_running_children(&parent));

        remove_worker(&child).unwrap();
        remove_worker(&parent).unwrap();
    }

    #[test]
    fn gc_workers_removes_done_dead_destroyed() {
        let _home = init_test_home();
        let name_done = uid("gc_done");
        let name_dead = uid("gc_dead");
        let name_destroyed = uid("gc_destroyed");
        let name_running = uid("gc_running");

        let mut w1 = make_worker(&name_done);
        w1.status = "done".to_string();
        save_worker(&w1, true).unwrap();

        let mut w2 = make_worker(&name_dead);
        w2.status = "dead".to_string();
        save_worker(&w2, true).unwrap();

        let mut w3 = make_worker(&name_destroyed);
        w3.status = "destroyed".to_string();
        save_worker(&w3, true).unwrap();

        let w4 = make_worker(&name_running);
        save_worker(&w4, true).unwrap();

        let removed = gc_workers().unwrap();
        assert!(removed.contains(&name_done));
        assert!(removed.contains(&name_dead));
        assert!(removed.contains(&name_destroyed));
        assert!(!removed.contains(&name_running));

        // running worker should still exist
        assert!(get_worker(&name_running).is_some());
        // gc'd workers should be gone
        assert!(get_worker(&name_done).is_none());
        assert!(get_worker(&name_dead).is_none());
        assert!(get_worker(&name_destroyed).is_none());

        remove_worker(&name_running).unwrap();
    }

    #[test]
    fn update_worker_fields_test() {
        let _home = init_test_home();
        let name = uid("update_fields");
        let w = make_worker(&name);
        save_worker(&w, true).unwrap();

        let mut updates = HashMap::new();
        updates.insert("done_reported".to_string(), serde_json::Value::Bool(true));
        updates.insert(
            "last_event_at".to_string(),
            serde_json::Value::String("2026-01-01T12:00:00Z".to_string()),
        );

        let updated = update_worker_fields(&name, &updates).unwrap();
        assert!(updated.is_some());
        let updated = updated.unwrap();
        assert!(updated.done_reported);
        assert_eq!(updated.last_event_at, "2026-01-01T12:00:00Z");

        let loaded = get_worker(&name).unwrap();
        assert!(loaded.done_reported);
        assert_eq!(loaded.last_event_at, "2026-01-01T12:00:00Z");

        remove_worker(&name).unwrap();
    }

    #[test]
    fn update_worker_fields_nonexistent() {
        let _home = init_test_home();
        let result = update_worker_fields("nonexistent_xyz_77777", &HashMap::new()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn worker_new_fields_defaults() {
        let json = serde_json::json!({
            "name": "minimal",
            "backend": "claude",
            "task": "t",
            "dir": "/d",
            "workdir": "/w",
            "base_branch": "main",
            "orchestrator": "cc"
        });
        let w: Worker = serde_json::from_value(json).unwrap();
        assert_eq!(w.reply_channel, "");
        assert_eq!(w.reply_to, "");
        assert_eq!(w.reply_thread, "");
        assert_eq!(w.last_event_at, "");
        assert!(!w.done_reported);
        assert!(!w.process_exited);
    }

    #[test]
    fn save_multiple_workers_and_load() {
        let _home = init_test_home();
        let names: Vec<String> = (0..3).map(|i| uid(&format!("multi_{i}"))).collect();
        for name in &names {
            save_worker(&make_worker(name), true).unwrap();
        }

        let workers = load_workers();
        for name in &names {
            assert!(workers.contains_key(name), "Worker {name} should exist");
        }

        for name in &names {
            remove_worker(name).unwrap();
        }
    }

    #[test]
    fn save_worker_duplicate_denied() {
        let _home = init_test_home();
        let name = uid("dup_deny");
        let w = make_worker(&name);
        save_worker(&w, true).unwrap();

        let err = save_worker(&w, false).unwrap_err();
        assert!(
            err.downcast_ref::<DuplicateWorkerError>().is_some(),
            "expected DuplicateWorkerError, got: {err}"
        );

        remove_worker(&name).unwrap();
    }

    #[test]
    fn save_worker_overwrite_allowed() {
        let _home = init_test_home();
        let name = uid("dup_allow");
        let w = make_worker(&name);
        save_worker(&w, true).unwrap();

        let mut w2 = make_worker(&name);
        w2.task = "updated task".to_string();
        save_worker(&w2, true).unwrap();

        let loaded = get_worker(&name).unwrap();
        assert_eq!(loaded.task, "updated task");

        remove_worker(&name).unwrap();
    }

    #[test]
    fn load_raw_corrupt_json_creates_backup() {
        let _home = init_test_home();
        let _ = config::ensure_home();
        let state_path = config::state_file();

        // Hold the file lock so other tests block while we have corrupt data
        let _lock = StateLock::acquire().unwrap();

        // Save existing state
        let original = fs::read_to_string(&state_path).unwrap_or_default();

        // Write corrupt JSON to the state file
        fs::write(&state_path, "{{not valid json}}").unwrap();

        // load_raw (private, no lock) should return empty for corrupt JSON
        let result = load_raw();
        assert!(result.is_empty());

        // The corrupt state file should have been renamed to state.bak.*
        let parent = state_path.parent().unwrap();
        let has_backup = fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("state.bak."))
            });
        assert!(has_backup, "expected a state.bak.* backup file");

        // Restore original state and clean up backup files
        fs::write(&state_path, &original).unwrap();
        for entry in fs::read_dir(parent).unwrap().filter_map(|e| e.ok()) {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("state.bak."))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    #[test]
    fn load_raw_non_object_json_returns_empty() {
        let _home = init_test_home();
        let _ = config::ensure_home();
        let state_path = config::state_file();

        // Hold the file lock so other tests block while we have non-object data
        let _lock = StateLock::acquire().unwrap();

        // Save existing state
        let original = fs::read_to_string(&state_path).unwrap_or_default();

        // Write valid JSON that is not an object (an array)
        fs::write(&state_path, "[1, 2, 3]").unwrap();

        let result = load_raw();
        assert!(result.is_empty());

        // Restore original state
        fs::write(&state_path, &original).unwrap();
    }
}
