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
mod tests;
