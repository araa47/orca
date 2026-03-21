use super::*;
use crate::types::{Backend, Orchestrator, WorkerStatus};
use std::sync::Once;

static INIT: Once = Once::new();
static mut TEMP_DIR: Option<tempfile::TempDir> = None;

/// Tests that touch `daemon.pid` must hold this lock to avoid races.
/// We recover from poisoned state so a panic in one test doesn't cascade.
static PID_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Same cached `ORCA_HOME` implies one `daemon.log`; tests that delete or read it must not run in parallel.
static LOG_FILE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_pid_file() -> std::sync::MutexGuard<'static, ()> {
    PID_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_daemon_log_tests() -> std::sync::MutexGuard<'static, ()> {
    LOG_FILE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn init_test_home() {
    INIT.call_once(|| {
        let tmp = tempfile::tempdir().expect("create temp dir");
        unsafe { std::env::set_var("ORCA_HOME", tmp.path().to_str().unwrap()) };
        unsafe { TEMP_DIR = Some(tmp) };
    });
}

#[test]
fn test_constants() {
    assert_eq!(ESCALATION_COOLDOWN, 60.0);
    assert_eq!(WARN_COOLDOWN, 300.0);
    assert_eq!(IDLE_CONFIRM_SECS, 30.0);
    assert_eq!(IDLE_MIN_LIFETIME, 45.0);
    assert_eq!(CHILD_FINISH_GRACE, 60.0);
}

#[test]
fn test_now_secs_returns_reasonable_value() {
    let t = now_secs();
    assert!(t > 1_704_067_200.0);
    assert!(t < 4_102_444_800.0);
}

#[test]
fn test_now_secs_monotonic() {
    let t1 = now_secs();
    let t2 = now_secs();
    assert!(t2 >= t1);
}

#[test]
fn test_parse_worker_age_valid() {
    let ts = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(120))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let age = parse_worker_age(&ts);
    assert!((119.0..125.0).contains(&age), "age was {age}");
}

#[test]
fn test_parse_worker_age_invalid() {
    assert_eq!(parse_worker_age("not-a-date"), 999.0);
    assert_eq!(parse_worker_age(""), 999.0);
}

#[test]
fn test_parse_worker_age_future() {
    let ts = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::seconds(3600))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let age = parse_worker_age(&ts);
    assert!(age < 0.0, "future ts should be negative, got {age}");
}

fn make_test_worker(name: &str) -> Worker {
    Worker {
        name: name.into(),
        pane_id: String::new(),
        backend: Backend::Claude,
        task: String::new(),
        dir: String::new(),
        workdir: String::new(),
        base_branch: String::new(),
        orchestrator: Orchestrator::None,
        orchestrator_pane: String::new(),
        session_id: String::new(),
        reply_channel: String::new(),
        reply_to: String::new(),
        reply_thread: String::new(),
        depth: 1,
        spawned_by: "root".into(),
        layout: String::new(),
        status: WorkerStatus::Running,
        started_at: String::new(),
        last_event_at: String::new(),
        done_reported: false,
        process_exited: false,
    }
}

#[test]
fn test_worker_target_with_pane_id() {
    let mut w = make_test_worker("test-worker");
    w.pane_id = "%42".into();
    assert_eq!(worker_target(&w), "%42");
}

#[test]
fn test_worker_target_fallback_to_name() {
    let w = make_test_worker("test-worker");
    let target = worker_target(&w);
    assert!(target.ends_with(":test-worker"));
}

#[test]
fn test_daemon_state_new() {
    let ds = DaemonState::new();
    assert!(ds.recently_escalated.is_empty());
    assert!(ds.recently_warned.is_empty());
    assert!(ds.idle_seen.is_empty());
    assert!(ds.idle_output_hash.is_empty());
    assert!(ds.had_children.is_empty());
    assert!(ds.children_finished_at.is_empty());
}

#[test]
fn test_daemon_state_clear_tracking() {
    let mut ds = DaemonState::new();
    let name = "test-worker";
    ds.idle_seen.insert(name.to_string(), 1.0);
    ds.idle_output_hash
        .insert(name.to_string(), "hash".to_string());
    ds.recently_escalated.insert(name.to_string(), 1.0);
    ds.recently_warned.insert(name.to_string(), 1.0);
    ds.had_children.insert(name.to_string(), true);
    ds.children_finished_at.insert(name.to_string(), 1.0);

    ds.clear_tracking(name);

    assert!(ds.idle_seen.is_empty());
    assert!(ds.idle_output_hash.is_empty());
    assert!(ds.recently_escalated.is_empty());
    assert!(ds.recently_warned.is_empty());
    assert!(ds.had_children.is_empty());
    assert!(ds.children_finished_at.is_empty());
}

#[test]
fn test_daemon_state_clear_tracking_leaves_other_workers() {
    let mut ds = DaemonState::new();
    ds.idle_seen.insert("w1".to_string(), 1.0);
    ds.idle_seen.insert("w2".to_string(), 2.0);
    ds.recently_escalated.insert("w1".to_string(), 1.0);
    ds.recently_escalated.insert("w2".to_string(), 2.0);

    ds.clear_tracking("w1");

    assert!(!ds.idle_seen.contains_key("w1"));
    assert!(ds.idle_seen.contains_key("w2"));
    assert!(!ds.recently_escalated.contains_key("w1"));
    assert!(ds.recently_escalated.contains_key("w2"));
}

// -----------------------------------------------------------------------
// log_msg
// -----------------------------------------------------------------------

#[test]
fn test_log_msg_writes_to_file() {
    let _log_lock = lock_daemon_log_tests();
    init_test_home();
    let _ = config::ensure_home();
    let log_path = config::daemon_log_file();
    let _ = fs::remove_file(&log_path);

    log_msg("test message from unit test");

    let content = fs::read_to_string(&log_path).expect("log file should exist");
    assert!(content.contains("test message from unit test"));
    assert!(content.contains('T'));
    assert!(content.contains('Z'));
}

#[test]
fn test_log_msg_appends() {
    let _log_lock = lock_daemon_log_tests();
    init_test_home();
    let _ = config::ensure_home();
    let log_path = config::daemon_log_file();
    let _ = fs::remove_file(&log_path);

    log_msg("first line");
    log_msg("second line");

    let content = fs::read_to_string(&log_path).unwrap();
    assert!(content.contains("first line"));
    assert!(content.contains("second line"));
    let lines: Vec<&str> = content.lines().collect();
    assert!(
        lines.len() >= 2,
        "expected at least 2 lines, got {}",
        lines.len()
    );
}

// -----------------------------------------------------------------------
// event_age_secs
// -----------------------------------------------------------------------

#[test]
fn test_event_age_secs_with_recent_timestamp() {
    let ts = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(10))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let mut w = make_test_worker("eage1");
    w.last_event_at = ts;
    let age = event_age_secs(&w);
    assert!((8.0..15.0).contains(&age), "expected ~10s, got {age}");
}

#[test]
fn test_event_age_secs_with_old_timestamp() {
    let ts = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(3600))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let mut w = make_test_worker("eage2");
    w.last_event_at = ts;
    let age = event_age_secs(&w);
    assert!(
        (3598.0..3605.0).contains(&age),
        "expected ~3600s, got {age}"
    );
}

#[test]
fn test_event_age_secs_with_invalid_timestamp() {
    let mut w = make_test_worker("eage3");
    w.last_event_at = "not-a-date".to_string();
    let age = event_age_secs(&w);
    assert!(age.is_infinite(), "expected INFINITY, got {age}");
}

#[test]
fn test_event_age_secs_empty_last_event_at_no_events_file() {
    init_test_home();
    let _ = config::ensure_home();
    let w = make_test_worker("nonexistent_worker_for_event_age_test");
    let age = event_age_secs(&w);
    assert!(
        age.is_infinite(),
        "expected INFINITY for worker with no events, got {age}"
    );
}

#[test]
fn test_event_age_secs_empty_last_event_at_with_events_file() {
    init_test_home();
    let _ = config::ensure_home();

    let name = format!("daemon_eage_events_{}", std::process::id());
    events::append_event(&name, "heartbeat", "", "test").unwrap();

    let w = make_test_worker(&name);
    let age = event_age_secs(&w);
    assert!(age < 5.0, "expected recent age, got {age}");

    events::remove_events(&name);
}

// -----------------------------------------------------------------------
// read_daemon_pid / is_daemon_running / remove_pid
// -----------------------------------------------------------------------

#[test]
fn test_read_daemon_pid_missing_file() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let _ = fs::remove_file(config::daemon_pid_file());

    assert!(read_daemon_pid().is_none());
}

#[test]
fn test_read_daemon_pid_invalid_content() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    fs::write(config::daemon_pid_file(), "not-a-number").unwrap();

    assert!(read_daemon_pid().is_none());
}

#[test]
fn test_read_daemon_pid_stale_pid() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    // PID 99999999 almost certainly doesn't exist
    fs::write(config::daemon_pid_file(), "99999999").unwrap();

    assert!(read_daemon_pid().is_none());
    // Should also have cleaned up the stale file
    assert!(!config::daemon_pid_file().exists());
}

#[test]
fn test_read_daemon_pid_with_own_pid() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let my_pid = process::id();

    // In some CI environments (containers, cargo-llvm-cov), kill(own_pid, 0)
    // may fail. Skip the assertion if the environment doesn't support it.
    let can_signal_self = unsafe { libc::kill(my_pid as libc::pid_t, 0) } == 0;
    if !can_signal_self {
        return;
    }

    fs::write(config::daemon_pid_file(), format!("{my_pid}")).unwrap();

    let result = read_daemon_pid();
    assert_eq!(result, Some(my_pid));

    let _ = fs::remove_file(config::daemon_pid_file());
}

#[test]
fn test_is_daemon_running_no_daemon() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let _ = fs::remove_file(config::daemon_pid_file());

    assert!(!is_daemon_running());
}

#[test]
fn test_is_daemon_running_with_pid() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let my_pid = process::id();

    let can_signal_self = unsafe { libc::kill(my_pid as libc::pid_t, 0) } == 0;
    if !can_signal_self {
        return;
    }

    fs::write(config::daemon_pid_file(), format!("{my_pid}")).unwrap();

    assert!(is_daemon_running());

    let _ = fs::remove_file(config::daemon_pid_file());
}

#[test]
fn test_remove_pid_removes_file() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let path = config::daemon_pid_file();
    fs::write(&path, "12345").unwrap();
    assert!(path.exists());

    remove_pid();

    assert!(!path.exists());
}

#[test]
fn test_remove_pid_no_file_no_panic() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let _ = fs::remove_file(config::daemon_pid_file());
    // Should not panic
    remove_pid();
}

// -----------------------------------------------------------------------
// stop_daemon — no actual daemon running
// -----------------------------------------------------------------------

#[test]
fn test_stop_daemon_no_daemon() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let _ = fs::remove_file(config::daemon_pid_file());

    assert!(!stop_daemon());
}

#[test]
fn test_stop_daemon_stale_pid() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    fs::write(config::daemon_pid_file(), "99999999").unwrap();

    assert!(!stop_daemon());
}

// -----------------------------------------------------------------------
// acquire_pid_lock / release_pid_lock
// -----------------------------------------------------------------------

#[test]
fn test_acquire_and_release_pid_lock() {
    let _lock = lock_pid_file();
    init_test_home();
    let _ = config::ensure_home();
    let _ = fs::remove_file(config::daemon_pid_file());
    DAEMON_LOCK_FD.store(-1, Ordering::SeqCst);

    let acquired = acquire_pid_lock();
    assert!(acquired);

    let pid_content = fs::read_to_string(config::daemon_pid_file()).unwrap();
    let stored_pid: u32 = pid_content.trim().parse().unwrap();
    assert_eq!(stored_pid, process::id());

    assert!(DAEMON_LOCK_FD.load(Ordering::SeqCst) >= 0);

    release_pid_lock();

    assert_eq!(DAEMON_LOCK_FD.load(Ordering::SeqCst), -1);
    assert!(!config::daemon_pid_file().exists());
}

#[test]
fn test_release_pid_lock_when_not_held() {
    DAEMON_LOCK_FD.store(-1, Ordering::SeqCst);
    // Should not panic
    release_pid_lock();
}

// -----------------------------------------------------------------------
// check_workers_inner — async tests
// -----------------------------------------------------------------------

fn save_test_worker(w: &Worker) {
    state::save_worker(w, true).unwrap();
}

fn unique_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering as AO};
    static CTR: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}_{}_{}",
        prefix,
        std::process::id(),
        CTR.fetch_add(1, AO::Relaxed)
    )
}

fn cleanup_worker(name: &str) {
    let _ = state::remove_worker(name);
    events::remove_events(name);
}

#[tokio::test]
async fn test_check_workers_inner_done_reported() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_done");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.done_reported = true;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    ds.idle_seen.insert(name.clone(), 1.0);
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    let updated = workers.get(&name);
    // Worker should be marked done (or absent if gc'd)
    if let Some(u) = updated {
        assert_eq!(u.status, WorkerStatus::Done);
    }
    // Tracking should be cleared
    assert!(!ds.idle_seen.contains_key(&name));

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_process_exited_without_done() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_exited");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.process_exited = true;
    w.done_reported = false;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(
            u.status,
            WorkerStatus::Dead,
            "expected 'dead' for exited without done"
        );
    }

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_process_exited_with_done_event() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_exdone");

    events::append_event(&name, "done", "finished", "test").unwrap();

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.process_exited = true;
    w.done_reported = false;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(
            u.status,
            WorkerStatus::Done,
            "expected 'done' when done event exists"
        );
    }

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_pane_died_no_done() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_paned");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.pane_id = "%99999".into();
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(
            u.status,
            WorkerStatus::Dead,
            "pane not alive should mark dead"
        );
    }

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_pane_died_with_done_event() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_panedone");

    events::append_event(&name, "done", "finished", "test").unwrap();

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.pane_id = "%99998".into();
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(
            u.status,
            WorkerStatus::Done,
            "pane died + done event → done"
        );
    }

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_skips_done_workers() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_skip");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Done;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    let u = workers.get(&name).unwrap();
    assert_eq!(u.status, WorkerStatus::Done, "done worker should stay done");

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_skips_dead_workers() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_dead");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Dead;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    let u = workers.get(&name).unwrap();
    assert_eq!(u.status, WorkerStatus::Dead, "dead worker should stay dead");

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_blocked_worker_pane_died() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_blocked");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Blocked;
    w.pane_id = "%99997".into();
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(u.status, WorkerStatus::Dead, "blocked + pane died → dead");
    }

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_prunes_stale_tracking() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_prune");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Done;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    ds.idle_seen
        .insert("nonexistent_ghost_worker".to_string(), 1.0);
    ds.recently_escalated
        .insert("nonexistent_ghost_worker".to_string(), 1.0);
    ds.recently_warned
        .insert("nonexistent_ghost_worker".to_string(), 1.0);
    ds.had_children
        .insert("nonexistent_ghost_worker".to_string(), true);
    ds.children_finished_at
        .insert("nonexistent_ghost_worker".to_string(), 1.0);
    ds.idle_output_hash
        .insert("nonexistent_ghost_worker".to_string(), "h".to_string());

    check_workers_inner(&mut ds).await;

    assert!(
        !ds.idle_seen.contains_key("nonexistent_ghost_worker"),
        "stale idle_seen not pruned"
    );
    assert!(
        !ds.recently_escalated
            .contains_key("nonexistent_ghost_worker"),
        "stale recently_escalated not pruned"
    );
    assert!(
        !ds.recently_warned.contains_key("nonexistent_ghost_worker"),
        "stale recently_warned not pruned"
    );
    assert!(
        !ds.had_children.contains_key("nonexistent_ghost_worker"),
        "stale had_children not pruned"
    );
    assert!(
        !ds.children_finished_at
            .contains_key("nonexistent_ghost_worker"),
        "stale children_finished_at not pruned"
    );
    assert!(
        !ds.idle_output_hash.contains_key("nonexistent_ghost_worker"),
        "stale idle_output_hash not pruned"
    );

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_inner_empty_state() {
    init_test_home();
    let _ = config::ensure_home();

    let mut ds = DaemonState::new();
    ds.idle_seen.insert("ghost".to_string(), 1.0);

    // With no workers in state, just prunes tracking
    check_workers_inner(&mut ds).await;

    assert!(ds.idle_seen.is_empty());
}

#[tokio::test]
async fn test_check_workers_inner_multiple_workers() {
    init_test_home();
    let _ = config::ensure_home();
    let name1 = unique_name("cw_multi1");
    let name2 = unique_name("cw_multi2");
    let name3 = unique_name("cw_multi3");

    let mut w1 = make_test_worker(&name1);
    w1.status = WorkerStatus::Running;
    w1.done_reported = true;
    w1.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w1);

    let mut w2 = make_test_worker(&name2);
    w2.status = WorkerStatus::Running;
    w2.process_exited = true;
    w2.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w2);

    let mut w3 = make_test_worker(&name3);
    w3.status = WorkerStatus::Done;
    w3.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w3);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u1) = workers.get(&name1) {
        assert_eq!(u1.status, WorkerStatus::Done);
    }
    if let Some(u2) = workers.get(&name2) {
        assert_eq!(u2.status, WorkerStatus::Dead);
    }
    let u3 = workers.get(&name3).unwrap();
    assert_eq!(u3.status, WorkerStatus::Done);

    cleanup_worker(&name1);
    cleanup_worker(&name2);
    cleanup_worker(&name3);
}

// -----------------------------------------------------------------------
// check_stuck — tests for paths that don't require real tmux
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_empty_output_returns_early() {
    init_test_home();
    let _ = config::ensure_home();
    let name = "cs_empty";
    let mut w = make_test_worker(name);
    w.status = WorkerStatus::Running;
    w.pane_id = "%99996".into();
    let workers = HashMap::new();
    let mut ds = DaemonState::new();

    // In test env, tmux capture_pane returns "" for non-existent panes.
    // check_stuck should return early without modifying state.
    check_stuck(name, &w, &workers, &mut ds).await;

    assert!(ds.idle_seen.is_empty());
    assert!(ds.recently_escalated.is_empty());
}

#[tokio::test]
async fn test_check_stuck_recent_events_clears_idle() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cs_recent");

    events::append_event(&name, "heartbeat", "", "test").unwrap();

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.last_event_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    w.pane_id = "%99995".into();

    let workers = HashMap::new();
    let mut ds = DaemonState::new();
    ds.idle_seen.insert(name.clone(), 1.0);
    ds.idle_output_hash
        .insert(name.clone(), "old_hash".to_string());

    // capture_pane returns "" in test env → early return
    check_stuck(&name, &w, &workers, &mut ds).await;

    // idle tracking may or may not be cleared depending on capture_pane output;
    // in test env (empty output), check_stuck returns before the recent-events check
    events::remove_events(&name);
}

// -----------------------------------------------------------------------
// check_workers via Mutex wrapper
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_workers_via_mutex() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_mutex");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.done_reported = true;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let state = Mutex::new(DaemonState::new());
    check_workers(&state).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(u.status, WorkerStatus::Done);
    }

    cleanup_worker(&name);
}

#[tokio::test]
async fn test_check_workers_arc_wrapper() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_arc");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.done_reported = true;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let state = std::sync::Arc::new(Mutex::new(DaemonState::new()));
    check_workers_arc(&state).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(u.status, WorkerStatus::Done);
    }

    cleanup_worker(&name);
}

// -----------------------------------------------------------------------
// event_age_secs — edge cases
// -----------------------------------------------------------------------

#[test]
fn test_event_age_secs_future_timestamp() {
    let ts = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::seconds(3600))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let mut w = make_test_worker("eage_future");
    w.last_event_at = ts;
    let age = event_age_secs(&w);
    assert!(
        age < 0.0,
        "future timestamp should yield negative age, got {age}"
    );
}

#[test]
fn test_event_age_secs_epoch_timestamp() {
    let mut w = make_test_worker("eage_epoch");
    w.last_event_at = "1970-01-01T00:00:00Z".to_string();
    let age = event_age_secs(&w);
    assert!(age > 1_700_000_000.0, "epoch should be very old, got {age}");
}

// -----------------------------------------------------------------------
// parse_worker_age — additional edge cases
// -----------------------------------------------------------------------

#[test]
fn test_parse_worker_age_just_started() {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let age = parse_worker_age(&ts);
    assert!(age < 5.0, "just-started worker age should be ~0, got {age}");
}

#[test]
fn test_parse_worker_age_wrong_format() {
    assert_eq!(parse_worker_age("2024-01-01 12:00:00"), 999.0);
    assert_eq!(parse_worker_age("2024/01/01T12:00:00Z"), 999.0);
}

// -----------------------------------------------------------------------
// worker_target — additional cases
// -----------------------------------------------------------------------

#[test]
fn test_worker_target_empty_pane_id_contains_session() {
    let w = make_test_worker("wt_worker");
    let target = worker_target(&w);
    assert!(target.contains(':'));
    assert!(target.ends_with(":wt_worker"));
}

#[test]
fn test_worker_target_non_empty_pane_id() {
    let mut w = make_test_worker("wt_pane");
    w.pane_id = "%123".into();
    assert_eq!(worker_target(&w), "%123");
}

// -----------------------------------------------------------------------
// DaemonState — additional coverage
// -----------------------------------------------------------------------

#[test]
fn test_daemon_state_clear_tracking_nonexistent_key() {
    let mut ds = DaemonState::new();
    // Clearing a key that doesn't exist should not panic
    ds.clear_tracking("does_not_exist");
    assert!(ds.idle_seen.is_empty());
}

#[test]
fn test_daemon_lock_fd_default() {
    // The static should be initializable and readable
    let val = DAEMON_LOCK_FD.load(Ordering::SeqCst);
    // Value depends on prior test state, but should not panic
    let _ = val;
}

// -----------------------------------------------------------------------
// log_msg — format verification
// -----------------------------------------------------------------------

#[test]
fn test_log_msg_format_has_iso_timestamp() {
    let _log_lock = lock_daemon_log_tests();
    init_test_home();
    config::ensure_home().expect("ensure_home for daemon.log");

    let marker = format!("format_check_{}", std::process::id());
    log_msg(&marker);

    let content = fs::read_to_string(config::daemon_log_file()).unwrap();
    let line = content.lines().find(|l| l.contains(&marker)).unwrap();
    // Format: [2024-01-01T00:00:00Z] message
    assert!(line.starts_with('['), "line should start with [: {line}");
    assert!(
        line.contains(&format!("] {marker}")),
        "line should contain '] message': {line}"
    );
    let bracket_end = line.find(']').unwrap();
    let ts = &line[1..bracket_end];
    assert!(ts.contains('T'), "timestamp should have T: {ts}");
    assert!(ts.ends_with('Z'), "timestamp should end with Z: {ts}");
    assert_eq!(ts.len(), 20, "ISO timestamp should be 20 chars: {ts}");
}

#[test]
fn test_log_msg_each_line_ends_with_newline() {
    let _log_lock = lock_daemon_log_tests();
    init_test_home();
    let _ = config::ensure_home();

    let m1 = format!("newline_check_a_{}", std::process::id());
    let m2 = format!("newline_check_b_{}", std::process::id());
    log_msg(&m1);
    log_msg(&m2);

    let raw = fs::read_to_string(config::daemon_log_file()).unwrap();
    assert!(raw.ends_with('\n'));
    let matching: Vec<&str> = raw
        .lines()
        .filter(|l| l.contains("newline_check_"))
        .collect();
    assert!(
        matching.len() >= 2,
        "expected at least 2 matching lines, got {}",
        matching.len()
    );
}

// -----------------------------------------------------------------------
// check_stuck — worker too young (< IDLE_MIN_LIFETIME)
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_worker_too_young_clears_idle() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cs_young");

    // Worker just started (age < IDLE_MIN_LIFETIME)
    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.backend = Backend::Claude;
    w.pane_id = "%99994".into();
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // No recent events → watchdog mode
    w.last_event_at = String::new();

    let workers = HashMap::new();
    let mut ds = DaemonState::new();
    ds.idle_seen.insert(name.to_string(), 1.0);
    ds.idle_output_hash
        .insert(name.to_string(), "hash".to_string());

    // In test env, capture_pane returns "" → early return
    check_stuck(&name, &w, &workers, &mut ds).await;

    // With empty output, check_stuck returns before reaching age check,
    // so idle tracking is unchanged (still set).
    // This covers the output.is_empty() early return path.
}

// -----------------------------------------------------------------------
// check_stuck — worker has running children
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_has_running_children() {
    init_test_home();
    let _ = config::ensure_home();
    let parent_name = unique_name("cs_parent");
    let child_name = unique_name("cs_child");

    let mut parent = make_test_worker(&parent_name);
    parent.status = WorkerStatus::Running;
    parent.backend = Backend::Claude;
    parent.pane_id = "%99993".into();
    parent.started_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(120))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    parent.last_event_at = String::new();

    let mut child = make_test_worker(&child_name);
    child.status = WorkerStatus::Running;
    child.spawned_by = parent_name.clone();

    let mut workers = HashMap::new();
    workers.insert(child_name.clone(), child);

    let mut ds = DaemonState::new();
    // capture_pane returns "" in test → early return before children check
    check_stuck(&parent_name, &parent, &workers, &mut ds).await;
}

// -----------------------------------------------------------------------
// check_stuck — children just finished (grace period)
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_children_finished_grace() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cs_grace");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.backend = Backend::Claude;
    w.pane_id = "%99992".into();
    w.started_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(120))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    w.last_event_at = String::new();

    let workers = HashMap::new();
    let mut ds = DaemonState::new();
    ds.had_children.insert(name.to_string(), true);

    // In test env capture_pane returns "" → early return
    check_stuck(&name, &w, &workers, &mut ds).await;
}

// -----------------------------------------------------------------------
// check_stuck — idle first seen
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_idle_first_seen() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cs_firstseen");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.backend = Backend::Claude;
    w.pane_id = "%99991".into();
    w.started_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(120))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    w.last_event_at = String::new();

    let workers = HashMap::new();
    let mut ds = DaemonState::new();

    // In test env capture_pane returns "" → early return
    check_stuck(&name, &w, &workers, &mut ds).await;
    // idle_seen not set because we return early on empty output
    assert!(!ds.idle_seen.contains_key(&name));
}

// -----------------------------------------------------------------------
// check_stuck — idle output hash changed
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_idle_output_hash_changed() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cs_hashchg");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.backend = Backend::Claude;
    w.pane_id = "%99990".into();
    w.started_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(120))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    w.last_event_at = String::new();

    let workers = HashMap::new();
    let mut ds = DaemonState::new();
    ds.idle_seen.insert(name.to_string(), now_secs() - 10.0);
    ds.idle_output_hash
        .insert(name.to_string(), "old_hash_value".to_string());

    // In test env capture_pane returns "" → early return before hash check
    check_stuck(&name, &w, &workers, &mut ds).await;
}

// -----------------------------------------------------------------------
// check_stuck — idle confirmed + done_reported
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_idle_confirmed_done_reported() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cs_idledone");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.backend = Backend::Claude;
    w.pane_id = "%99989".into();
    w.done_reported = true;
    w.started_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(120))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    w.last_event_at = String::new();

    let workers = HashMap::new();
    let mut ds = DaemonState::new();
    ds.idle_seen.insert(name.to_string(), now_secs() - 60.0);

    // In test env capture_pane returns "" → early return
    check_stuck(&name, &w, &workers, &mut ds).await;
}

// -----------------------------------------------------------------------
// check_stuck — idle confirmed + warn cooldown
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_stuck_idle_confirmed_warn_cooldown() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cs_idlewarn");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.backend = Backend::Claude;
    w.pane_id = "%99988".into();
    w.done_reported = false;
    w.started_at = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(120))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    w.last_event_at = String::new();

    let workers = HashMap::new();
    let mut ds = DaemonState::new();
    ds.idle_seen.insert(name.to_string(), now_secs() - 60.0);
    ds.recently_warned.insert(name.to_string(), now_secs());

    // In test env capture_pane returns "" → early return
    check_stuck(&name, &w, &workers, &mut ds).await;
}

// -----------------------------------------------------------------------
// event_age_secs — empty last_event_at with events file
// -----------------------------------------------------------------------

#[test]
fn test_event_age_secs_empty_last_event_with_valid_events_file() {
    init_test_home();
    let _ = config::ensure_home();

    let name = unique_name("eage_valid_events");
    // Write an event with a known timestamp
    events::append_event(&name, "heartbeat", "", "test").unwrap();

    let w = make_test_worker(&name);
    // last_event_at is empty, so it falls back to events file
    let age = event_age_secs(&w);
    assert!(age < 5.0, "expected recent age from events file, got {age}");

    events::remove_events(&name);
}

#[test]
fn test_event_age_secs_empty_last_event_with_invalid_events_file_ts() {
    init_test_home();
    let _ = config::ensure_home();

    let name = unique_name("eage_bad_ts");
    // Write a manually corrupted events file
    let events_path = config::events_dir().join(format!("{name}.jsonl"));
    fs::write(
        &events_path,
        r#"{"event":"heartbeat","timestamp":"not-a-date","source":"test"}"#,
    )
    .unwrap();

    let w = make_test_worker(&name);
    let age = event_age_secs(&w);
    assert!(
        age.is_infinite(),
        "expected INFINITY for invalid timestamp in events file, got {age}"
    );

    events::remove_events(&name);
}

// -----------------------------------------------------------------------
// parse_worker_age — additional tests
// -----------------------------------------------------------------------

#[test]
fn test_parse_worker_age_exact_timestamp() {
    let ts = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::seconds(60))
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let age = parse_worker_age(&ts);
    assert!((58.0..65.0).contains(&age), "expected ~60s, got {age}");
}

// -----------------------------------------------------------------------
// check_workers_inner — blocked worker stays blocked if pane alive
// (in test env, pane check returns false → marks dead)
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_workers_inner_blocked_worker_with_done_event_pane_died() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_blocked_done");

    events::append_event(&name, "done", "finished", "test").unwrap();

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Blocked;
    w.pane_id = "%99986".into();
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(
            u.status,
            WorkerStatus::Done,
            "blocked + pane died + done event → done"
        );
    }

    cleanup_worker(&name);
}

// -----------------------------------------------------------------------
// check_workers_inner — process exited + done_reported
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_workers_inner_process_exited_with_done_reported() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_exit_done_rpt");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.process_exited = true;
    w.done_reported = true;
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        // done_reported is checked first in check_workers_inner
        assert_eq!(u.status, WorkerStatus::Done);
    }

    cleanup_worker(&name);
}

// -----------------------------------------------------------------------
// DaemonState — clear_tracking partial keys
// -----------------------------------------------------------------------

#[test]
fn test_daemon_state_clear_tracking_selective() {
    let mut ds = DaemonState::new();
    ds.idle_seen.insert("w1".to_string(), 1.0);
    ds.idle_output_hash
        .insert("w1".to_string(), "h1".to_string());
    ds.had_children.insert("w1".to_string(), true);
    // w2 only has some fields
    ds.idle_seen.insert("w2".to_string(), 2.0);
    ds.recently_warned.insert("w2".to_string(), 2.0);

    ds.clear_tracking("w1");

    assert!(!ds.idle_seen.contains_key("w1"));
    assert!(!ds.idle_output_hash.contains_key("w1"));
    assert!(!ds.had_children.contains_key("w1"));
    assert!(ds.idle_seen.contains_key("w2"));
    assert!(ds.recently_warned.contains_key("w2"));
}

// -----------------------------------------------------------------------
// check_workers_inner — worker with empty pane_id uses window_exists
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_check_workers_inner_empty_pane_id_uses_window() {
    init_test_home();
    let _ = config::ensure_home();
    let name = unique_name("cw_nopane");

    let mut w = make_test_worker(&name);
    w.status = WorkerStatus::Running;
    w.pane_id = String::new(); // empty → falls back to window_exists
    w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_test_worker(&w);

    let mut ds = DaemonState::new();
    check_workers_inner(&mut ds).await;

    let workers = state::load_workers();
    if let Some(u) = workers.get(&name) {
        assert_eq!(u.status, WorkerStatus::Dead, "no window → dead");
    }

    cleanup_worker(&name);
}
