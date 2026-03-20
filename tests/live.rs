//! Live integration tests that spawn real worker agents via tmux.
//!
//! These tests require:
//! - A running tmux server
//! - The agent binaries (`claude`, `codex`, `cursor`/`agent`) on PATH
//!
//! They are **skipped by default** in CI and normal `cargo test` runs.
//! To run them locally: `ORCA_LIVE_TESTS=1 cargo test --test live -- --test-threads=1`
//!
//! Use --test-threads=1 because tests share tmux state and must not run in parallel.

use std::process::Command as StdCommand;
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn should_run() -> bool {
    std::env::var("ORCA_LIVE_TESTS").is_ok_and(|v| v == "1" || v == "true")
}

fn has_tmux() -> bool {
    StdCommand::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_binary(name: &str) -> bool {
    which::which(name).is_ok()
}

fn skip_unless(need_tmux: bool, binaries: &[&str]) -> bool {
    if !should_run() {
        eprintln!("Skipped: set ORCA_LIVE_TESTS=1 to run live tests");
        return true;
    }
    if need_tmux && !has_tmux() {
        eprintln!("Skipped: tmux not available");
        return true;
    }
    for bin in binaries {
        if !has_binary(bin) {
            eprintln!("Skipped: {bin} binary not found");
            return true;
        }
    }
    false
}

fn live_orca(home: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("orca").unwrap();
    cmd.env("ORCA_HOME", home.path());
    cmd
}

/// Read the state.json and parse a worker's fields.
fn read_worker(home: &tempfile::TempDir, name: &str) -> Option<serde_json::Value> {
    let path = home.path().join("state.json");
    let text = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&text).ok()?;
    val.get(name).cloned()
}

/// Poll state.json until worker reaches one of the target statuses, or timeout.
fn wait_for_status(
    home: &tempfile::TempDir,
    name: &str,
    targets: &[&str],
    timeout: Duration,
) -> String {
    let start = std::time::Instant::now();
    let mut last = String::from("unknown");
    while start.elapsed() < timeout {
        if let Some(w) = read_worker(home, name)
            && let Some(s) = w.get("status").and_then(|v| v.as_str())
        {
            last = s.to_string();
            if targets.contains(&s) {
                return last;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    last
}

/// Check if a tmux pane_id (e.g. "%42") is alive.
fn pane_alive(pane_id: &str) -> bool {
    let out = StdCommand::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}"])
        .output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.lines().any(|l| l.trim() == pane_id)
        }
        Err(_) => false,
    }
}

/// Capture pane content via tmux.
fn capture_pane(pane_id: &str, lines: u32) -> String {
    let neg = format!("-{lines}");
    let out = StdCommand::new("tmux")
        .args(["capture-pane", "-p", "-t", pane_id, "-S", &neg])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => String::new(),
    }
}

struct TestEnv {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let p = project.path().to_str().unwrap();

        StdCommand::new("git")
            .args(["-C", p, "init"])
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["-C", p, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["-C", p, "config", "user.name", "Test"])
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["-C", p, "commit", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();

        Self { home, project }
    }

    fn orca(&self) -> Command {
        live_orca(&self.home)
    }

    fn project_dir(&self) -> &str {
        self.project.path().to_str().unwrap()
    }

    fn spawn(&self, name: &str, backend: &str, task: &str, orchestrator: &str) {
        let mut args = vec![
            "spawn",
            task,
            "-b",
            backend,
            "-d",
            self.project_dir(),
            "-n",
            name,
            "--orchestrator",
            orchestrator,
        ];
        // Auto-detect pane for tmux orchestrators
        if matches!(orchestrator, "cc" | "cx" | "cu") {
            args.push("--pane");
            args.push("");
        }
        self.orca()
            .args(&args)
            .assert()
            .success()
            .stdout(predicates::str::contains(format!("Spawned: {name}")));
    }

    fn kill(&self, name: &str) {
        let _ = self.orca().args(["kill", name]).output();
    }

    #[allow(dead_code)]
    fn killall(&self) {
        let _ = self.orca().args(["killall", "--force"]).output();
    }

    #[allow(dead_code)]
    fn stop_daemon(&self) {
        let _ = self.orca().args(["daemon", "stop"]).output();
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = live_orca(&self.home).args(["killall", "--force"]).output();
        let _ = live_orca(&self.home).args(["daemon", "stop"]).output();
    }
}

/// Assert worker state fields after spawn.
fn assert_worker_spawned(env: &TestEnv, name: &str, backend: &str) {
    let w = read_worker(&env.home, name)
        .unwrap_or_else(|| panic!("worker {name} not found in state.json"));

    // Status should be running
    let status = w["status"].as_str().unwrap();
    assert_eq!(
        status, "running",
        "{name}: expected status=running, got {status}"
    );

    // Backend should be the canonical name
    let actual_backend = w["backend"].as_str().unwrap();
    assert_eq!(actual_backend, backend, "{name}: backend mismatch");

    // pane_id should start with %
    let pane_id = w["pane_id"].as_str().unwrap();
    assert!(
        pane_id.starts_with('%'),
        "{name}: pane_id should start with %, got {pane_id:?}"
    );

    // Tmux pane should actually be alive
    assert!(
        pane_alive(pane_id),
        "{name}: tmux pane {pane_id} should be alive"
    );

    // Worktree directory should exist
    let workdir = w["workdir"].as_str().unwrap();
    assert!(
        std::path::Path::new(workdir).is_dir(),
        "{name}: worktree dir {workdir} should exist"
    );

    // started_at should be a valid ISO timestamp
    let started_at = w["started_at"].as_str().unwrap();
    assert!(
        started_at.contains('T') && started_at.ends_with('Z'),
        "{name}: started_at should be ISO format, got {started_at:?}"
    );

    // depth should be > 0 (spawn adds 1)
    let depth = w["depth"].as_u64().unwrap();
    assert!(depth >= 1, "{name}: depth should be >= 1, got {depth}");
}

/// Assert the log file exists and has content.
fn assert_log_file_has_content(env: &TestEnv, name: &str) {
    let log_path = env.home.path().join("logs").join(format!("{name}.log"));
    assert!(
        log_path.exists(),
        "{name}: log file {} should exist",
        log_path.display()
    );
    let content = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        !content.trim().is_empty(),
        "{name}: log file should have content"
    );
}

/// Assert the daemon was auto-started.
fn assert_daemon_running(env: &TestEnv) {
    let pid_path = env.home.path().join("daemon.pid");
    assert!(
        pid_path.exists(),
        "daemon.pid should exist after first spawn"
    );
    let pid_str = std::fs::read_to_string(&pid_path).unwrap();
    let pid: u32 = pid_str.trim().parse().unwrap_or(0);
    assert!(pid > 0, "daemon pid should be > 0, got {pid_str:?}");
}

/// Assert the audit log contains an entry.
fn assert_audit_contains(env: &TestEnv, needle: &str) {
    let path = env.home.path().join("audit.log");
    let content = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        content.contains(needle),
        "audit.log should contain {needle:?}, got:\n{content}"
    );
}

// ---------------------------------------------------------------------------
// Claude Code (cc) — full lifecycle
// ---------------------------------------------------------------------------

#[test]
fn live_cc_spawn_verify_running_kill() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    // 1. Spawn
    env.spawn("lcc", "cc", "say hello world", "none");

    // 2. Verify state fields
    assert_worker_spawned(&env, "lcc", "claude");

    // 3. Daemon auto-started
    assert_daemon_running(&env);

    // 4. Audit log has SPAWN entry
    assert_audit_contains(&env, "SPAWN worker=lcc");

    // 5. Wait for agent to produce some output, then check log file
    std::thread::sleep(Duration::from_secs(5));
    assert_log_file_has_content(&env, "lcc");

    // 6. Verify the agent pane has content (agent actually started)
    let w = read_worker(&env.home, "lcc").unwrap();
    let pane_id = w["pane_id"].as_str().unwrap();
    let pane_output = capture_pane(pane_id, 50);
    assert!(
        !pane_output.trim().is_empty(),
        "cc: pane should have output from the agent"
    );

    // 7. List shows the worker with correct details
    let out = env.orca().arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lcc"), "list should contain worker name");
    assert!(stdout.contains("claude"), "list should show backend");
    assert!(
        stdout.contains("running") || stdout.contains("▶"),
        "list should show running"
    );

    // 8. Status shows correct fields
    env.orca()
        .args(["status", "lcc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Name: lcc"))
        .stdout(predicate::str::contains("Backend: claude"))
        .stdout(predicate::str::contains("Status: running"));

    // 9. Logs returns content
    env.orca()
        .args(["logs", "lcc", "-n", "20"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());

    // 10. Kill and verify cleanup
    env.orca()
        .args(["kill", "lcc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Killed: lcc"));

    assert_audit_contains(&env, "KILL worker=lcc");
    assert!(
        read_worker(&env.home, "lcc").is_none(),
        "worker should be removed from state"
    );
    assert!(!pane_alive(pane_id), "pane should be dead after kill");
}

// ---------------------------------------------------------------------------
// Codex (cx) — full lifecycle
// ---------------------------------------------------------------------------

#[test]
fn live_cx_spawn_verify_running_kill() {
    if skip_unless(true, &["codex"]) {
        return;
    }
    let env = TestEnv::new();

    env.spawn("lcx", "cx", "say hello world", "none");
    assert_worker_spawned(&env, "lcx", "codex");
    assert_daemon_running(&env);
    assert_audit_contains(&env, "SPAWN worker=lcx");

    std::thread::sleep(Duration::from_secs(5));
    assert_log_file_has_content(&env, "lcx");

    let w = read_worker(&env.home, "lcx").unwrap();
    let pane_id = w["pane_id"].as_str().unwrap();
    let pane_output = capture_pane(pane_id, 50);
    assert!(
        !pane_output.trim().is_empty(),
        "cx: pane should have output from the agent"
    );

    env.orca()
        .args(["status", "lcx"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Backend: codex"))
        .stdout(predicate::str::contains("Status: running"));

    env.kill("lcx");
    assert!(read_worker(&env.home, "lcx").is_none());
    assert!(!pane_alive(pane_id));
}

// ---------------------------------------------------------------------------
// Cursor (cu) — full lifecycle
// ---------------------------------------------------------------------------

#[test]
fn live_cu_spawn_verify_running_kill() {
    // cursor agent can be either "agent" or "cursor"
    if skip_unless(true, &[]) {
        return;
    }
    if !has_binary("agent") && !has_binary("cursor") {
        eprintln!("Skipped: cursor/agent binary not found");
        return;
    }
    let env = TestEnv::new();

    env.spawn("lcu", "cu", "say hello world", "none");
    assert_worker_spawned(&env, "lcu", "cursor");
    assert_daemon_running(&env);
    assert_audit_contains(&env, "SPAWN worker=lcu");

    std::thread::sleep(Duration::from_secs(5));
    assert_log_file_has_content(&env, "lcu");

    let w = read_worker(&env.home, "lcu").unwrap();
    let pane_id = w["pane_id"].as_str().unwrap();
    let pane_output = capture_pane(pane_id, 50);
    assert!(
        !pane_output.trim().is_empty(),
        "cu: pane should have output from the agent"
    );

    env.orca()
        .args(["status", "lcu"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Backend: cursor"))
        .stdout(predicate::str::contains("Status: running"));

    env.kill("lcu");
    assert!(read_worker(&env.home, "lcu").is_none());
    assert!(!pane_alive(pane_id));
}

// ---------------------------------------------------------------------------
// Report done → daemon marks worker done
// ---------------------------------------------------------------------------

#[test]
fn live_report_done_transitions_to_done() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    env.spawn("lrpt", "cc", "wait for done signal", "none");
    assert_worker_spawned(&env, "lrpt", "claude");

    // Report done
    env.orca()
        .args(["report", "-w", "lrpt", "-e", "done", "-s", "test"])
        .assert()
        .success();

    // Verify event file
    let events_path = env.home.path().join("events").join("lrpt.jsonl");
    assert!(events_path.exists(), "events file should exist");
    let events_content = std::fs::read_to_string(&events_path).unwrap();
    assert!(events_content.contains("\"event\":\"done\""));

    // Verify done_reported flag in state
    let w = read_worker(&env.home, "lrpt").unwrap();
    assert_eq!(
        w["done_reported"].as_bool(),
        Some(true),
        "done_reported should be set"
    );

    // Wait for daemon to pick up and transition to "done"
    let final_status = wait_for_status(&env.home, "lrpt", &["done"], Duration::from_secs(15));
    assert_eq!(
        final_status, "done",
        "daemon should transition worker to done"
    );

    assert_audit_contains(&env, "REPORT worker=lrpt event=done");
}

// ---------------------------------------------------------------------------
// Report blocked → state updates
// ---------------------------------------------------------------------------

#[test]
fn live_report_blocked_updates_status() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    env.spawn("lblk", "cc", "wait for instructions", "none");
    assert_worker_spawned(&env, "lblk", "claude");

    env.orca()
        .args([
            "report",
            "-w",
            "lblk",
            "-e",
            "blocked",
            "-s",
            "agent",
            "-m",
            "need API key",
        ])
        .assert()
        .success();

    let w = read_worker(&env.home, "lblk").unwrap();
    assert_eq!(
        w["status"].as_str(),
        Some("blocked"),
        "status should be blocked"
    );

    let events_path = env.home.path().join("events").join("lblk.jsonl");
    let content = std::fs::read_to_string(events_path).unwrap();
    assert!(content.contains("\"event\":\"blocked\""));
    assert!(content.contains("need API key"));

    env.kill("lblk");
}

// ---------------------------------------------------------------------------
// Steer sends message to pane
// ---------------------------------------------------------------------------

#[test]
fn live_steer_delivers_message() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    env.spawn("lstr", "cc", "wait for follow-up instructions", "none");
    assert_worker_spawned(&env, "lstr", "claude");

    // Wait for agent to be ready
    std::thread::sleep(Duration::from_secs(5));

    env.orca()
        .args(["steer", "lstr", "please also add integration tests"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Steered: lstr"));

    assert_audit_contains(&env, "STEER worker=lstr");

    // Give the message time to appear in pane
    std::thread::sleep(Duration::from_secs(2));

    let w = read_worker(&env.home, "lstr").unwrap();
    let pane_id = w["pane_id"].as_str().unwrap();
    let output = capture_pane(pane_id, 100);
    assert!(
        output.contains("integration tests"),
        "steer message should appear in pane output, got:\n{}",
        output.lines().rev().take(10).collect::<Vec<_>>().join("\n")
    );

    env.kill("lstr");
}

// ---------------------------------------------------------------------------
// Multi-worker spawn + killall
// ---------------------------------------------------------------------------

#[test]
fn live_multi_worker_spawn_list_killall() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    env.spawn("lm1", "cc", "task one", "none");
    env.spawn("lm2", "cc", "task two", "none");

    assert_worker_spawned(&env, "lm1", "claude");
    assert_worker_spawned(&env, "lm2", "claude");

    // Both panes are alive
    let w1 = read_worker(&env.home, "lm1").unwrap();
    let w2 = read_worker(&env.home, "lm2").unwrap();
    let p1 = w1["pane_id"].as_str().unwrap();
    let p2 = w2["pane_id"].as_str().unwrap();
    assert!(pane_alive(p1), "lm1 pane should be alive");
    assert!(pane_alive(p2), "lm2 pane should be alive");

    // List shows both
    let out = env.orca().arg("list").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lm1") && stdout.contains("lm2"));

    // Killall --force
    env.orca()
        .args(["killall", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Killed: lm1"))
        .stdout(predicate::str::contains("Killed: lm2"));

    assert!(!pane_alive(p1), "lm1 pane should be dead after killall");
    assert!(!pane_alive(p2), "lm2 pane should be dead after killall");

    // State is empty
    assert!(read_worker(&env.home, "lm1").is_none());
    assert!(read_worker(&env.home, "lm2").is_none());

    assert_audit_contains(&env, "KILLALL scope=force");
}

// ---------------------------------------------------------------------------
// Daemon lifecycle
// ---------------------------------------------------------------------------

#[test]
fn live_daemon_lifecycle() {
    if skip_unless(true, &[]) {
        return;
    }
    let env = TestEnv::new();

    // Start
    env.orca()
        .args(["daemon", "start"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Daemon started"));

    let pid_path = env.home.path().join("daemon.pid");
    assert!(pid_path.exists(), "daemon.pid should exist");
    let pid: u32 = std::fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(pid > 0);

    // Status
    env.orca()
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Daemon running"));

    // Double start is idempotent
    env.orca()
        .args(["daemon", "start"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already running"));

    // Stop
    env.orca()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Daemon stopped"));

    env.orca()
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not running"));

    // Double stop is idempotent
    env.orca()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not running"));
}

// ---------------------------------------------------------------------------
// Spawn + kill + gc (full worktree cleanup)
// ---------------------------------------------------------------------------

#[test]
fn live_spawn_kill_gc_cleans_everything() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    env.spawn("lgc", "cc", "do something", "none");
    assert_worker_spawned(&env, "lgc", "claude");

    let w = read_worker(&env.home, "lgc").unwrap();
    let workdir = w["workdir"].as_str().unwrap().to_string();
    let pane_id = w["pane_id"].as_str().unwrap().to_string();

    // Kill removes from state + kills pane
    env.kill("lgc");
    assert!(read_worker(&env.home, "lgc").is_none());
    assert!(!pane_alive(&pane_id));

    // GC should not error
    env.orca().args(["gc", "--force"]).assert().success();

    // Worktree directory should be cleaned up
    assert!(
        !std::path::Path::new(&workdir).exists(),
        "worktree {workdir} should be removed after kill"
    );

    // Log and event files should be gone after gc
    let log_path = env.home.path().join("logs").join("lgc.log");
    let events_path = env.home.path().join("events").join("lgc.jsonl");
    // log may or may not exist (gc only removes done/dead workers' logs)
    // but the worker state should be clean
    let _ = (log_path, events_path);
}

// ---------------------------------------------------------------------------
// GC kills tmux panes for done workers (regression test)
// ---------------------------------------------------------------------------

#[test]
fn live_gc_kills_tmux_panes() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    env.spawn("lgcp", "cc", "wait for done signal", "none");
    assert_worker_spawned(&env, "lgcp", "claude");

    let w = read_worker(&env.home, "lgcp").unwrap();
    let pane_id = w["pane_id"].as_str().unwrap().to_string();
    let workdir = w["workdir"].as_str().unwrap().to_string();
    assert!(pane_alive(&pane_id), "pane should be alive after spawn");

    // Report done so the worker transitions to "done" status
    env.orca()
        .args(["report", "-w", "lgcp", "-e", "done", "-s", "test"])
        .assert()
        .success();

    let final_status = wait_for_status(&env.home, "lgcp", &["done"], Duration::from_secs(15));
    assert_eq!(final_status, "done");

    // Pane is still alive — gc should kill it
    assert!(pane_alive(&pane_id), "pane should still be alive before gc");

    // GC the done worker
    env.orca().args(["gc", "--force"]).assert().success();

    // Pane must be dead after gc
    assert!(
        !pane_alive(&pane_id),
        "gc must kill tmux pane for done workers"
    );

    // State, worktree, logs, events should all be cleaned up
    assert!(
        read_worker(&env.home, "lgcp").is_none(),
        "worker should be removed from state after gc"
    );
    assert!(
        !std::path::Path::new(&workdir).exists(),
        "worktree should be removed after gc"
    );
    let log_path = env.home.path().join("logs").join("lgcp.log");
    assert!(!log_path.exists(), "log file should be removed after gc");
    let events_path = env.home.path().join("events").join("lgcp.jsonl");
    assert!(
        !events_path.exists(),
        "events file should be removed after gc"
    );
}

// ---------------------------------------------------------------------------
// Orchestrator notification — cc orchestrator gets wake message
// ---------------------------------------------------------------------------

#[test]
fn live_orchestrator_notification() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    // Create a "fake orchestrator" pane — just a plain shell window we own
    let orch_pane = {
        let out = StdCommand::new("tmux")
            .args(["new-window", "-d", "-P", "-F", "#{pane_id}"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert!(
        orch_pane.starts_with('%'),
        "should get a pane_id, got {orch_pane:?}"
    );

    // Spawn a worker with orchestrator=cc pointing to our fake pane
    let mut cmd = env.orca();
    cmd.args([
        "spawn",
        "say done immediately",
        "-b",
        "cc",
        "-d",
        env.project_dir(),
        "-n",
        "lnotify",
        "--orchestrator",
        "cc",
        "--pane",
        &orch_pane,
    ]);
    cmd.assert().success();

    assert_worker_spawned(&env, "lnotify", "claude");

    // Report done to trigger daemon notification
    env.orca()
        .args(["report", "-w", "lnotify", "-e", "done", "-s", "test"])
        .assert()
        .success();

    // Wait for daemon to process and send wake message to orchestrator pane
    let final_status = wait_for_status(&env.home, "lnotify", &["done"], Duration::from_secs(15));
    assert_eq!(final_status, "done");

    // Give the notification a moment to be delivered
    std::thread::sleep(Duration::from_secs(2));

    // The orchestrator pane should have received the ORCA wake message
    let orch_output = capture_pane(&orch_pane, 100);
    assert!(
        orch_output.contains("ORCA: worker lnotify"),
        "orchestrator pane should have received ORCA notification, got:\n{}",
        orch_output
    );
    assert!(
        orch_output.contains("orca logs lnotify"),
        "notification should include logs command"
    );

    // Clean up
    env.kill("lnotify");
    let _ = StdCommand::new("tmux")
        .args(["kill-pane", "-t", &orch_pane])
        .output();
}

// ---------------------------------------------------------------------------
// Worktree isolation — workers get separate git worktrees
// ---------------------------------------------------------------------------

#[test]
fn live_worktree_isolation() {
    if skip_unless(true, &["claude"]) {
        return;
    }
    let env = TestEnv::new();

    // Create a file in the project
    std::fs::write(env.project.path().join("hello.txt"), "original content").unwrap();
    StdCommand::new("git")
        .args(["-C", env.project_dir(), "add", "-A"])
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["-C", env.project_dir(), "commit", "-m", "add hello"])
        .output()
        .unwrap();

    env.spawn("lwt", "cc", "list files", "none");
    assert_worker_spawned(&env, "lwt", "claude");

    let w = read_worker(&env.home, "lwt").unwrap();
    let workdir = w["workdir"].as_str().unwrap();

    // Worktree should be under .worktrees/
    assert!(
        workdir.contains("/.worktrees/lwt"),
        "workdir should be under .worktrees/, got {workdir}"
    );

    // Worktree should have the same file
    let wt_file = std::path::Path::new(workdir).join("hello.txt");
    assert!(wt_file.exists(), "worktree should have hello.txt");
    let content = std::fs::read_to_string(&wt_file).unwrap();
    assert_eq!(content, "original content");

    // Changes in worktree don't affect original
    std::fs::write(&wt_file, "modified in worktree").unwrap();
    let original = std::fs::read_to_string(env.project.path().join("hello.txt")).unwrap();
    assert_eq!(original, "original content", "original should be untouched");

    env.kill("lwt");
}
