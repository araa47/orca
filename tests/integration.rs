use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

fn orca_cmd() -> Command {
    Command::cargo_bin("orca").unwrap()
}

fn orca_with_home(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = orca_cmd();
    cmd.env("ORCA_HOME", tmp.path())
        .env_remove("TMUX")
        .env("ORCA_TMUX_SESSION", "test-session");
    cmd
}

fn seed_worker(tmp: &tempfile::TempDir, name: &str, status: &str) {
    let state_path = tmp.path().join("state.json");
    let existing: serde_json::Value = if state_path.exists() {
        let text = fs::read_to_string(&state_path).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    let mut map = match existing {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    map.insert(
        name.to_string(),
        serde_json::json!({
            "name": name,
            "backend": "claude",
            "task": format!("task for {name}"),
            "dir": "/tmp/test",
            "workdir": "/tmp/test",
            "base_branch": "main",
            "orchestrator": "none",
            "orchestrator_pane": "",
            "session_id": "",
            "pane_id": "",
            "depth": 0,
            "spawned_by": "",
            "layout": "window",
            "status": status,
            "started_at": "2026-01-01T00:00:00Z",
            "last_event_at": "",
            "done_reported": false,
            "process_exited": false,
        }),
    );
    fs::create_dir_all(tmp.path()).unwrap();
    fs::write(&state_path, serde_json::to_string_pretty(&map).unwrap()).unwrap();
}

fn seed_worker_full(
    tmp: &tempfile::TempDir,
    name: &str,
    status: &str,
    spawned_by: &str,
    depth: u32,
) {
    let state_path = tmp.path().join("state.json");
    let existing: serde_json::Value = if state_path.exists() {
        let text = fs::read_to_string(&state_path).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    let mut map = match existing {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    map.insert(
        name.to_string(),
        serde_json::json!({
            "name": name,
            "backend": "claude",
            "task": format!("task for {name}"),
            "dir": "/tmp/test",
            "workdir": "/tmp/test",
            "base_branch": "main",
            "orchestrator": "none",
            "orchestrator_pane": "",
            "session_id": "",
            "pane_id": "",
            "depth": depth,
            "spawned_by": spawned_by,
            "layout": "window",
            "status": status,
            "started_at": "2026-01-01T00:00:00Z",
            "last_event_at": "",
            "done_reported": false,
            "process_exited": false,
        }),
    );
    fs::create_dir_all(tmp.path()).unwrap();
    fs::write(&state_path, serde_json::to_string_pretty(&map).unwrap()).unwrap();
}

// -----------------------------------------------------------------------
// Basic CLI tests
// -----------------------------------------------------------------------

#[test]
fn test_help_flag() {
    orca_cmd().arg("--help").assert().success();
}

#[test]
fn test_no_args_shows_help() {
    orca_cmd().assert().failure();
}

// -----------------------------------------------------------------------
// list
// -----------------------------------------------------------------------

#[test]
fn test_list_empty_state() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No workers"));
}

#[test]
fn test_list_with_workers() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "alpha", "running");
    seed_worker(&tmp, "beta", "done");

    orca_with_home(&tmp)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha"))
        .stdout(predicate::str::contains("beta"));
}

#[test]
fn test_list_shows_tree_structure() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker_full(&tmp, "parent", "running", "", 0);
    seed_worker_full(&tmp, "child", "running", "parent", 1);

    orca_with_home(&tmp)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("parent"))
        .stdout(predicate::str::contains("child"));
}

// -----------------------------------------------------------------------
// status
// -----------------------------------------------------------------------

#[test]
fn test_status_nonexistent_worker() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["status", "ghost-worker"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_status_existing_worker() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "alive-worker", "running");

    orca_with_home(&tmp)
        .args(["status", "alive-worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Name: alive-worker"))
        .stdout(predicate::str::contains("Status: running"))
        .stdout(predicate::str::contains("Backend: claude"));
}

#[test]
fn test_status_shows_children() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker_full(&tmp, "boss", "running", "", 0);
    seed_worker_full(&tmp, "minion-a", "running", "boss", 1);
    seed_worker_full(&tmp, "minion-b", "done", "boss", 1);

    orca_with_home(&tmp)
        .args(["status", "boss"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Children:"))
        .stdout(predicate::str::contains("minion-a"))
        .stdout(predicate::str::contains("minion-b"));
}

#[test]
fn test_status_done_worker_no_last_output() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "done-worker", "done");

    orca_with_home(&tmp)
        .args(["status", "done-worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Name: done-worker"))
        .stdout(predicate::str::contains("Status: done"));
}

// -----------------------------------------------------------------------
// logs
// -----------------------------------------------------------------------

#[test]
fn test_logs_nonexistent_worker() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["logs", "ghost-worker"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_logs_from_log_file() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "log-worker", "done");

    let logs_dir = tmp.path().join("logs");
    fs::create_dir_all(&logs_dir).unwrap();
    fs::write(
        logs_dir.join("log-worker.log"),
        "line 1\nline 2\nline 3\nline 4\nline 5\n",
    )
    .unwrap();

    orca_with_home(&tmp)
        .args(["logs", "log-worker"])
        .assert()
        .success()
        .stdout(predicate::str::contains("line 1"))
        .stdout(predicate::str::contains("line 5"));
}

#[test]
fn test_logs_with_lines_flag() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "log-tail", "done");

    let logs_dir = tmp.path().join("logs");
    fs::create_dir_all(&logs_dir).unwrap();
    let content: String = (1..=100).map(|i| format!("log line {i}\n")).collect();
    fs::write(logs_dir.join("log-tail.log"), &content).unwrap();

    orca_with_home(&tmp)
        .args(["logs", "log-tail", "-n", "3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("log line 100"))
        .stdout(predicate::str::contains("log line 98"));
}

#[test]
fn test_logs_strips_ansi_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "ansi-worker", "done");

    let logs_dir = tmp.path().join("logs");
    fs::create_dir_all(&logs_dir).unwrap();
    fs::write(
        logs_dir.join("ansi-worker.log"),
        "\x1b[31mred text\x1b[0m\n",
    )
    .unwrap();

    let output = orca_with_home(&tmp)
        .args(["logs", "ansi-worker"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("red text"));
    assert!(!stdout.contains("\x1b[31m"));
}

#[test]
fn test_logs_raw_preserves_ansi() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "raw-worker", "done");

    let logs_dir = tmp.path().join("logs");
    fs::create_dir_all(&logs_dir).unwrap();
    fs::write(logs_dir.join("raw-worker.log"), "\x1b[31mred text\x1b[0m\n").unwrap();

    let output = orca_with_home(&tmp)
        .args(["logs", "raw-worker", "--raw"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\x1b[31m"));
}

// -----------------------------------------------------------------------
// kill
// -----------------------------------------------------------------------

#[test]
fn test_kill_nonexistent_worker() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["kill", "ghost-worker"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_kill_existing_worker() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "doomed", "running");

    orca_with_home(&tmp)
        .args(["kill", "doomed"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Killed: doomed"));

    let state_text = fs::read_to_string(tmp.path().join("state.json")).unwrap();
    assert!(!state_text.contains("\"doomed\""));
}

// -----------------------------------------------------------------------
// steer
// -----------------------------------------------------------------------

#[test]
fn test_steer_nonexistent_worker() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["steer", "ghost-worker", "do", "something"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_steer_done_worker() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "finished", "done");

    orca_with_home(&tmp)
        .args(["steer", "finished", "keep", "going"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running/blocked"));
}

// -----------------------------------------------------------------------
// report
// -----------------------------------------------------------------------

#[test]
fn test_report_invalid_event_type() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["report", "--worker", "w1", "--event", "invalid_event"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid event"));
}

#[test]
fn test_report_nonexistent_worker() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["report", "--worker", "ghost", "--event", "done"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_report_valid_event() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "reporting-worker", "running");
    fs::create_dir_all(tmp.path().join("events")).unwrap();

    orca_with_home(&tmp)
        .args([
            "report",
            "--worker",
            "reporting-worker",
            "--event",
            "heartbeat",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Reported: reporting-worker heartbeat",
        ));
}

#[test]
fn test_report_done_event_updates_state() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "done-rpt", "running");
    fs::create_dir_all(tmp.path().join("events")).unwrap();

    orca_with_home(&tmp)
        .args(["report", "--worker", "done-rpt", "--event", "done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Reported: done-rpt done"));

    let events_path = tmp.path().join("events").join("done-rpt.jsonl");
    let events_text = fs::read_to_string(&events_path).unwrap();
    assert!(events_text.contains("\"done\""));
}

#[test]
fn test_report_hook_done_deferred_when_subworkers_running() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "par-hook-def", "running");
    seed_worker_full(&tmp, "kid-hook-def", "running", "par-hook-def", 1);
    fs::create_dir_all(tmp.path().join("events")).unwrap();

    orca_with_home(&tmp)
        .args([
            "report",
            "--worker",
            "par-hook-def",
            "--event",
            "done",
            "--source",
            "hook",
            "--message",
            "claude stop",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "done deferred while sub-workers run",
        ));

    let events_path = tmp.path().join("events").join("par-hook-def.jsonl");
    let events_text = fs::read_to_string(&events_path).unwrap();
    assert!(
        events_text.contains("\"heartbeat\""),
        "expected heartbeat, got: {events_text}",
    );
    assert!(
        !events_text.contains("\"done\""),
        "hook done should not be recorded: {events_text}",
    );
}

#[test]
fn test_report_blocked_event_updates_status() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "block-rpt", "running");
    fs::create_dir_all(tmp.path().join("events")).unwrap();

    orca_with_home(&tmp)
        .args([
            "report",
            "--worker",
            "block-rpt",
            "--event",
            "blocked",
            "--message",
            "waiting for review",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Reported: block-rpt blocked"));

    let events_path = tmp.path().join("events").join("block-rpt.jsonl");
    let events_text = fs::read_to_string(&events_path).unwrap();
    assert!(events_text.contains("\"blocked\""));
}

#[test]
fn test_report_with_message_and_source() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "msg-worker", "running");
    fs::create_dir_all(tmp.path().join("events")).unwrap();

    orca_with_home(&tmp)
        .args([
            "report",
            "--worker",
            "msg-worker",
            "--event",
            "process_exit",
            "--message",
            "exit code 0",
            "--source",
            "wrapper",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Reported: msg-worker process_exit",
        ));
}

// -----------------------------------------------------------------------
// killall
// -----------------------------------------------------------------------

#[test]
fn test_killall_without_flags() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .arg("killall")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--mine"))
        .stderr(predicate::str::contains("--force"));
}

#[test]
fn test_killall_force_empty() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["killall", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No workers"));
}

#[test]
fn test_killall_force_with_workers() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "victim-1", "running");
    seed_worker(&tmp, "victim-2", "blocked");

    orca_with_home(&tmp)
        .args(["killall", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Killed:"));

    let state_text = fs::read_to_string(tmp.path().join("state.json")).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_text).unwrap();
    assert!(state.as_object().unwrap().is_empty());
}

#[test]
fn test_killall_mine_outside_tmux() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "w1", "running");

    orca_with_home(&tmp)
        .args(["killall", "--mine"])
        .assert()
        .success();
}

#[test]
fn test_killall_by_session_id() {
    let tmp = tempfile::tempdir().unwrap();
    let state_path = tmp.path().join("state.json");
    fs::create_dir_all(tmp.path()).unwrap();
    fs::write(
        &state_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "w-sess": {
                "name": "w-sess",
                "backend": "claude",
                "task": "task",
                "dir": "/tmp",
                "workdir": "/tmp",
                "base_branch": "main",
                "orchestrator": "none",
                "orchestrator_pane": "",
                "session_id": "my-session-123",
                "pane_id": "",
                "depth": 0,
                "spawned_by": "",
                "layout": "window",
                "status": "running",
                "started_at": "2026-01-01T00:00:00Z",
                "last_event_at": "",
                "done_reported": false,
                "process_exited": false
            },
            "w-other": {
                "name": "w-other",
                "backend": "claude",
                "task": "other task",
                "dir": "/tmp",
                "workdir": "/tmp",
                "base_branch": "main",
                "orchestrator": "none",
                "orchestrator_pane": "",
                "session_id": "different-session",
                "pane_id": "",
                "depth": 0,
                "spawned_by": "",
                "layout": "window",
                "status": "running",
                "started_at": "2026-01-01T00:00:00Z",
                "last_event_at": "",
                "done_reported": false,
                "process_exited": false
            }
        }))
        .unwrap(),
    )
    .unwrap();

    orca_with_home(&tmp)
        .args(["killall", "--session-id", "my-session-123"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Killed: w-sess"));

    let state_text = fs::read_to_string(&state_path).unwrap();
    assert!(!state_text.contains("\"w-sess\"") || !state_text.contains("\"name\": \"w-sess\""));
    assert!(state_text.contains("w-other"));
}

// -----------------------------------------------------------------------
// gc
// -----------------------------------------------------------------------

#[test]
fn test_gc_without_flags() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .arg("gc")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--mine"))
        .stderr(predicate::str::contains("--force"));
}

#[test]
fn test_gc_force_empty() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["gc", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to clean"));
}

#[test]
fn test_gc_force_with_done_workers() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "done-1", "done");
    seed_worker(&tmp, "dead-1", "dead");
    seed_worker(&tmp, "running-1", "running");
    fs::create_dir_all(tmp.path().join("events")).unwrap();
    fs::create_dir_all(tmp.path().join("logs")).unwrap();

    orca_with_home(&tmp)
        .args(["gc", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cleaned:"));

    let state_text = fs::read_to_string(tmp.path().join("state.json")).unwrap();
    assert!(!state_text.contains("done-1"));
    assert!(!state_text.contains("dead-1"));
    assert!(state_text.contains("running-1"));
}

#[test]
fn test_gc_cleans_event_and_log_files() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "cleanup-me", "done");
    let events_dir = tmp.path().join("events");
    let logs_dir = tmp.path().join("logs");
    fs::create_dir_all(&events_dir).unwrap();
    fs::create_dir_all(&logs_dir).unwrap();
    fs::write(events_dir.join("cleanup-me.jsonl"), "{}\n").unwrap();
    fs::write(logs_dir.join("cleanup-me.log"), "some logs\n").unwrap();

    orca_with_home(&tmp)
        .args(["gc", "--force"])
        .assert()
        .success();

    assert!(!events_dir.join("cleanup-me.jsonl").exists());
    assert!(!logs_dir.join("cleanup-me.log").exists());
}

#[test]
fn test_gc_force_with_destroyed_workers() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "destroyed-1", "destroyed");
    fs::create_dir_all(tmp.path().join("events")).unwrap();
    fs::create_dir_all(tmp.path().join("logs")).unwrap();

    orca_with_home(&tmp)
        .args(["gc", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cleaned:"));
}

#[test]
fn test_gc_mine_outside_tmux() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "w1", "done");
    fs::create_dir_all(tmp.path().join("events")).unwrap();
    fs::create_dir_all(tmp.path().join("logs")).unwrap();

    orca_with_home(&tmp)
        .args(["gc", "--mine"])
        .assert()
        .success();
}

// -----------------------------------------------------------------------
// pane
// -----------------------------------------------------------------------

#[test]
fn test_pane_outside_tmux() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .arg("pane")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not inside a tmux session"));
}

// -----------------------------------------------------------------------
// daemon
// -----------------------------------------------------------------------

#[test]
fn test_daemon_status_no_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not running"));
}

#[test]
fn test_daemon_stop_no_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not running"));
}

// -----------------------------------------------------------------------
// hooks
// -----------------------------------------------------------------------

#[test]
fn test_hooks_install_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = orca_with_home(&tmp).args(["hooks", "install"]).assert();
}

#[test]
fn test_hooks_uninstall_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = orca_with_home(&tmp).args(["hooks", "uninstall"]).assert();
}

// -----------------------------------------------------------------------
// spawn error paths
// -----------------------------------------------------------------------

#[test]
fn test_spawn_max_depth_exceeded() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .env("ORCA_MAX_DEPTH", "1")
        .env("ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR", "1")
        .args([
            "spawn",
            "do something",
            "--depth",
            "1",
            "--spawned-by",
            "root",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("max orchestration depth"));
}

#[test]
fn test_spawn_rejects_orchestrator_none_without_opt_in() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .env("ORCA_MAX_DEPTH", "3")
        .args([
            "spawn",
            "do something",
            "--depth",
            "0",
            "--spawned-by",
            "root",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--orchestrator is required"));
}

#[test]
fn test_spawn_rejects_unknown_orchestrator() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args([
            "spawn",
            "do something",
            "--orchestrator",
            "typo",
            "--spawned-by",
            "root",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown --orchestrator"));
}

#[test]
fn test_spawn_rejects_unknown_spawned_by() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .env("ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR", "1")
        .args([
            "spawn",
            "do something",
            "--spawned-by",
            "nonexistent-parent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not match any tracked worker",
        ));
}

#[test]
fn test_spawn_openclaw_rejects_without_reply_routing() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .args([
            "spawn",
            "do something",
            "--orchestrator",
            "openclaw",
            "--spawned-by",
            "root",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--reply-channel and --reply-to"));
}

#[test]
fn test_spawn_stale_orca_worker_name_without_spawned_by() {
    let tmp = tempfile::tempdir().unwrap();
    orca_with_home(&tmp)
        .env("ORCA_WORKER_NAME", "stale-dead-worker")
        .env("ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR", "1")
        .args(["spawn", "do something", "--spawned-by", "root"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not in Orca state"));
}

#[test]
fn test_spawn_missing_task() {
    orca_cmd()
        .args(["spawn"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// -----------------------------------------------------------------------
// audit log written by commands
// -----------------------------------------------------------------------

#[test]
fn test_kill_writes_audit_log() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "audit-victim", "running");

    orca_with_home(&tmp)
        .args(["kill", "audit-victim"])
        .assert()
        .success();

    let audit_path = tmp.path().join("audit.log");
    if audit_path.exists() {
        let content = fs::read_to_string(&audit_path).unwrap();
        assert!(content.contains("KILL"));
        assert!(content.contains("audit-victim"));
    }
}

#[test]
fn test_report_writes_audit_log() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "audit-rpt", "running");
    fs::create_dir_all(tmp.path().join("events")).unwrap();

    orca_with_home(&tmp)
        .args(["report", "--worker", "audit-rpt", "--event", "heartbeat"])
        .assert()
        .success();

    let audit_path = tmp.path().join("audit.log");
    if audit_path.exists() {
        let content = fs::read_to_string(&audit_path).unwrap();
        assert!(content.contains("REPORT"));
        assert!(content.contains("audit-rpt"));
    }
}

#[test]
fn test_killall_writes_audit_log() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "ka-victim", "running");

    orca_with_home(&tmp)
        .args(["killall", "--force"])
        .assert()
        .success();

    let audit_path = tmp.path().join("audit.log");
    if audit_path.exists() {
        let content = fs::read_to_string(&audit_path).unwrap();
        assert!(content.contains("KILLALL"));
    }
}

#[test]
fn test_gc_writes_audit_log() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "gc-victim", "done");
    fs::create_dir_all(tmp.path().join("events")).unwrap();
    fs::create_dir_all(tmp.path().join("logs")).unwrap();

    orca_with_home(&tmp)
        .args(["gc", "--force"])
        .assert()
        .success();

    let audit_path = tmp.path().join("audit.log");
    if audit_path.exists() {
        let content = fs::read_to_string(&audit_path).unwrap();
        assert!(content.contains("GC"));
    }
}

// -----------------------------------------------------------------------
// steer additional cases
// -----------------------------------------------------------------------

#[test]
fn test_steer_dead_worker() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "dead-w", "dead");

    orca_with_home(&tmp)
        .args(["steer", "dead-w", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running/blocked"));
}

#[test]
fn test_steer_destroyed_worker() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "destroyed-w", "destroyed");

    orca_with_home(&tmp)
        .args(["steer", "destroyed-w", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running/blocked"));
}

// -----------------------------------------------------------------------
// Interaction tests for complex state scenarios
// -----------------------------------------------------------------------

#[test]
fn test_gc_force_only_cleans_terminal_statuses() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "r1", "running");
    seed_worker(&tmp, "b1", "blocked");
    seed_worker(&tmp, "d1", "done");
    seed_worker(&tmp, "x1", "dead");
    seed_worker(&tmp, "y1", "destroyed");
    fs::create_dir_all(tmp.path().join("events")).unwrap();
    fs::create_dir_all(tmp.path().join("logs")).unwrap();

    orca_with_home(&tmp)
        .args(["gc", "--force"])
        .assert()
        .success();

    let state_text = fs::read_to_string(tmp.path().join("state.json")).unwrap();
    assert!(state_text.contains("r1"));
    assert!(state_text.contains("b1"));
    assert!(!state_text.contains("\"d1\"") || !state_text.contains("\"name\": \"d1\""));
}

#[test]
fn test_list_all_status_icons() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker(&tmp, "r", "running");
    seed_worker(&tmp, "b", "blocked");
    seed_worker(&tmp, "d", "done");
    seed_worker(&tmp, "x", "dead");
    seed_worker(&tmp, "y", "destroyed");

    let output = orca_with_home(&tmp).arg("list").output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▶") || stdout.contains("running"));
    assert!(stdout.contains("⏸") || stdout.contains("blocked"));
    assert!(stdout.contains("✓") || stdout.contains("done"));
    assert!(stdout.contains("✗") || stdout.contains("dead"));
}

#[test]
fn test_status_spawned_by_field() {
    let tmp = tempfile::tempdir().unwrap();
    seed_worker_full(&tmp, "parent-orc", "running", "", 0);
    seed_worker_full(&tmp, "child-wrk", "running", "parent-orc", 1);

    orca_with_home(&tmp)
        .args(["status", "child-wrk"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Spawned by: parent-orc"));
}

// -----------------------------------------------------------------------
// Edge cases for report
// -----------------------------------------------------------------------

#[test]
fn test_report_all_valid_events() {
    for event in &["done", "blocked", "heartbeat", "process_exit"] {
        let tmp = tempfile::tempdir().unwrap();
        seed_worker(&tmp, "evt-worker", "running");
        fs::create_dir_all(tmp.path().join("events")).unwrap();

        orca_with_home(&tmp)
            .args(["report", "--worker", "evt-worker", "--event", event])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "Reported: evt-worker {event}"
            )));
    }
}

// -----------------------------------------------------------------------
// killall --pane
// -----------------------------------------------------------------------

#[test]
fn test_killall_pane_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let state_path = tmp.path().join("state.json");
    fs::create_dir_all(tmp.path()).unwrap();
    fs::write(
        &state_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "pane-w": {
                "name": "pane-w",
                "backend": "claude",
                "task": "task",
                "dir": "/tmp",
                "workdir": "/tmp",
                "base_branch": "main",
                "orchestrator": "none",
                "orchestrator_pane": "%42",
                "session_id": "",
                "pane_id": "",
                "depth": 0,
                "spawned_by": "",
                "layout": "window",
                "status": "running",
                "started_at": "2026-01-01T00:00:00Z",
                "last_event_at": "",
                "done_reported": false,
                "process_exited": false
            },
            "other-w": {
                "name": "other-w",
                "backend": "claude",
                "task": "other",
                "dir": "/tmp",
                "workdir": "/tmp",
                "base_branch": "main",
                "orchestrator": "none",
                "orchestrator_pane": "%99",
                "session_id": "",
                "pane_id": "",
                "depth": 0,
                "spawned_by": "",
                "layout": "window",
                "status": "running",
                "started_at": "2026-01-01T00:00:00Z",
                "last_event_at": "",
                "done_reported": false,
                "process_exited": false
            }
        }))
        .unwrap(),
    )
    .unwrap();

    orca_with_home(&tmp)
        .args(["killall", "--pane", "%42"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Killed: pane-w"));

    let state_text = fs::read_to_string(&state_path).unwrap();
    assert!(state_text.contains("other-w"));
}
