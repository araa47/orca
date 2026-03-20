//! Append-only worker event store — one JSONL file per worker under ORCA_HOME/events/.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};

use fs2::FileExt;
use serde_json::json;

use crate::config;

pub const VALID_EVENTS: &[&str] = &["done", "blocked", "heartbeat", "process_exit"];

fn events_path(worker_name: &str) -> std::path::PathBuf {
    config::events_dir().join(format!("{worker_name}.jsonl"))
}

/// Append an event to the worker's JSONL log. Returns the event dict.
pub fn append_event(
    worker_name: &str,
    event: &str,
    message: &str,
    source: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if !VALID_EVENTS.contains(&event) {
        return Err(format!(
            "Invalid event type: {event:?} (valid: {})",
            VALID_EVENTS.join(", ")
        )
        .into());
    }

    config::ensure_home()?;

    let path = events_path(worker_name);
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.lock_exclusive()?;

    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut record = json!({
        "event": event,
        "timestamp": ts,
        "source": source,
    });
    if !message.is_empty() {
        record["message"] = json!(message);
    }
    let line = serde_json::to_string(&record)? + "\n";
    let mut writer = std::io::BufWriter::new(&file);
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    file.sync_all()?;

    Ok(record)
}

/// Read all events for a worker. Returns empty vec if no file exists.
pub fn read_events(worker_name: &str) -> Vec<serde_json::Value> {
    let path = events_path(worker_name);
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    if file.lock_shared().is_err() {
        return Vec::new();
    }

    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            events.push(val);
        }
    }
    events
}

/// Check if the worker has emitted at least one 'done' event.
pub fn has_done_event(worker_name: &str) -> bool {
    read_events(worker_name)
        .iter()
        .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("done"))
}

/// Return the timestamp of the most recent event, or empty string.
pub fn last_event_time(worker_name: &str) -> String {
    let events = read_events(worker_name);
    events
        .last()
        .and_then(|e| e.get("timestamp"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Delete the event log for a worker (called during gc).
pub fn remove_events(worker_name: &str) {
    let path = events_path(worker_name);
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();
    static mut TEMP_DIR: Option<tempfile::TempDir> = None;

    fn init_test_home() {
        INIT.call_once(|| {
            let tmp = tempfile::tempdir().expect("create temp dir");
            unsafe { std::env::set_var("ORCA_HOME", tmp.path().to_str().unwrap()) };
            unsafe { TEMP_DIR = Some(tmp) };
        });
    }

    #[test]
    fn test_valid_events_list() {
        assert!(VALID_EVENTS.contains(&"done"));
        assert!(VALID_EVENTS.contains(&"blocked"));
        assert!(VALID_EVENTS.contains(&"heartbeat"));
        assert!(VALID_EVENTS.contains(&"process_exit"));
        assert!(!VALID_EVENTS.contains(&"invalid"));
    }

    #[test]
    fn test_append_and_read_events() {
        init_test_home();
        let name = format!("test_events_{}", std::process::id());

        let record = append_event(&name, "done", "finished", "hook").unwrap();
        assert_eq!(record["event"], "done");
        assert_eq!(record["message"], "finished");
        assert_eq!(record["source"], "hook");
        assert!(record["timestamp"].as_str().unwrap().contains('T'));

        let events = read_events(&name);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "done");

        remove_events(&name);
    }

    #[test]
    fn test_append_invalid_event() {
        init_test_home();
        let result = append_event("test", "invalid_event", "", "hook");
        assert!(result.is_err());
    }

    #[test]
    fn test_has_done_event() {
        init_test_home();
        let name = format!("test_has_done_{}", std::process::id());

        assert!(!has_done_event(&name));

        append_event(&name, "heartbeat", "", "hook").unwrap();
        assert!(!has_done_event(&name));

        append_event(&name, "done", "", "hook").unwrap();
        assert!(has_done_event(&name));

        remove_events(&name);
    }

    #[test]
    fn test_last_event_time() {
        init_test_home();
        let name = format!("test_last_time_{}", std::process::id());

        assert_eq!(last_event_time(&name), "");

        append_event(&name, "heartbeat", "", "hook").unwrap();
        let ts = last_event_time(&name);
        assert!(!ts.is_empty());
        assert!(ts.contains('T'));

        remove_events(&name);
    }

    #[test]
    fn test_read_events_nonexistent_worker() {
        let events = read_events("nonexistent_worker_xyz_99999");
        assert!(events.is_empty());
    }

    #[test]
    fn test_remove_events() {
        init_test_home();
        let name = format!("test_remove_events_{}", std::process::id());

        append_event(&name, "done", "", "hook").unwrap();
        assert!(!read_events(&name).is_empty());

        remove_events(&name);
        assert!(read_events(&name).is_empty());
    }

    #[test]
    fn test_append_event_empty_message() {
        init_test_home();
        let name = format!("test_empty_msg_{}", std::process::id());

        let record = append_event(&name, "blocked", "", "agent").unwrap();
        assert!(record.get("message").is_none());
        assert_eq!(record["source"], "agent");

        remove_events(&name);
    }

    #[test]
    fn test_read_events_skips_malformed_lines() {
        init_test_home();
        let name = format!("test_malformed_{}", std::process::id());
        let _ = config::ensure_home();
        let path = events_path(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Write a mix of valid JSON, invalid JSON, empty lines, and whitespace
        let content = concat!(
            r#"{"event":"heartbeat","timestamp":"2026-01-01T00:00:00Z","source":"hook"}"#,
            "\n",
            "this is not json\n",
            "\n",
            "   \n",
            r#"{"event":"done","timestamp":"2026-01-01T00:01:00Z","source":"agent"}"#,
            "\n",
            "{malformed json}\n",
            r#"{"event":"blocked","timestamp":"2026-01-01T00:02:00Z","source":"hook","message":"waiting"}"#,
            "\n",
        );
        fs::write(&path, content).unwrap();

        let events = read_events(&name);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["event"], "heartbeat");
        assert_eq!(events[1]["event"], "done");
        assert_eq!(events[2]["event"], "blocked");
        assert_eq!(events[2]["message"], "waiting");

        remove_events(&name);
    }

    #[test]
    fn test_multiple_events() {
        init_test_home();
        let name = format!("test_multi_{}", std::process::id());

        append_event(&name, "heartbeat", "", "hook").unwrap();
        append_event(&name, "blocked", "waiting for input", "agent").unwrap();
        append_event(&name, "done", "all done", "agent").unwrap();

        let events = read_events(&name);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["event"], "heartbeat");
        assert_eq!(events[1]["event"], "blocked");
        assert_eq!(events[2]["event"], "done");

        remove_events(&name);
    }
}
