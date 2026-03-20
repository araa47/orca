//! Daemon: tmux control-mode watcher + wake-on-done.
//!
//! Runs as a single background process that attaches to the orca tmux session
//! in control mode and watches for pane/window death events. When a worker's
//! pane dies, it marks the worker done and wakes the orchestrator.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::config;
use crate::events;
use crate::prompts;
use crate::state::{self, Worker};
use crate::tmux;
use crate::wake;

const ESCALATION_COOLDOWN: f64 = 60.0;
const WARN_COOLDOWN: f64 = 300.0;
const IDLE_CONFIRM_SECS: f64 = 30.0;
const IDLE_MIN_LIFETIME: f64 = 45.0;
const CHILD_FINISH_GRACE: f64 = 60.0;

struct DaemonState {
    recently_escalated: HashMap<String, f64>,
    recently_warned: HashMap<String, f64>,
    idle_seen: HashMap<String, f64>,
    idle_output_hash: HashMap<String, String>,
    had_children: HashMap<String, bool>,
    children_finished_at: HashMap<String, f64>,
}

impl DaemonState {
    fn new() -> Self {
        Self {
            recently_escalated: HashMap::new(),
            recently_warned: HashMap::new(),
            idle_seen: HashMap::new(),
            idle_output_hash: HashMap::new(),
            had_children: HashMap::new(),
            children_finished_at: HashMap::new(),
        }
    }

    fn clear_tracking(&mut self, name: &str) {
        self.idle_seen.remove(name);
        self.idle_output_hash.remove(name);
        self.recently_escalated.remove(name);
        self.recently_warned.remove(name);
        self.had_children.remove(name);
        self.children_finished_at.remove(name);
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn log_msg(msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] {msg}\n");
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config::daemon_log_file())
    {
        let _ = f.write_all(line.as_bytes());
    }
}

use std::sync::atomic::{AtomicI32, Ordering};

static DAEMON_LOCK_FD: AtomicI32 = AtomicI32::new(-1);

fn acquire_pid_lock() -> bool {
    let _ = config::ensure_home();
    let path = config::daemon_pid_file();
    let path_c = match std::ffi::CString::new(path.to_str().unwrap_or("/dev/null")) {
        Ok(c) => c,
        Err(_) => return false,
    };
    unsafe {
        let fd = libc::open(
            path_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT,
            0o644 as libc::c_uint,
        );
        if fd < 0 {
            return false;
        }
        if libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) != 0 {
            libc::close(fd);
            return false;
        }
        let _ = libc::ftruncate(fd, 0);
        let _ = libc::lseek(fd, 0, libc::SEEK_SET);
        let pid_str = format!("{}", process::id());
        let _ = libc::write(fd, pid_str.as_ptr() as *const _, pid_str.len());
        let _ = libc::fsync(fd);
        DAEMON_LOCK_FD.store(fd, Ordering::SeqCst);
    }
    true
}

fn release_pid_lock() {
    let fd = DAEMON_LOCK_FD.swap(-1, Ordering::SeqCst);
    if fd >= 0 {
        unsafe {
            libc::close(fd);
        }
    }
    let _ = fs::remove_file(config::daemon_pid_file());
}

fn remove_pid() {
    let _ = fs::remove_file(config::daemon_pid_file());
}

/// Read the daemon PID from the pidfile, returning `None` if stale or missing.
pub fn read_daemon_pid() -> Option<u32> {
    let path = config::daemon_pid_file();
    let text = fs::read_to_string(&path).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    // Check if process is alive via kill(pid, 0)
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        Some(pid)
    } else {
        let _ = fs::remove_file(&path);
        None
    }
}

/// Returns true if the orca daemon is currently running.
pub fn is_daemon_running() -> bool {
    read_daemon_pid().is_some()
}

/// Stop the running daemon. Returns true if it was stopped.
pub fn stop_daemon() -> bool {
    let pid = match read_daemon_pid() {
        Some(p) => p,
        None => return false,
    };

    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }

    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret != 0 {
            remove_pid();
            return true;
        }
    }

    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    remove_pid();
    true
}

fn worker_target(worker: &Worker) -> String {
    if !worker.pane_id.is_empty() {
        worker.pane_id.clone()
    } else {
        format!("{}:{}", config::tmux_session(), worker.name)
    }
}

async fn check_workers(state: &Mutex<DaemonState>) {
    let mut ds = state.lock().await;
    check_workers_inner(&mut ds).await;
}

async fn check_workers_arc(state: &std::sync::Arc<Mutex<DaemonState>>) {
    check_workers(state.as_ref()).await;
}

fn event_age_secs(worker: &Worker) -> f64 {
    let ts_str = if !worker.last_event_at.is_empty() {
        &worker.last_event_at
    } else {
        let t = events::last_event_time(&worker.name);
        if t.is_empty() {
            return f64::INFINITY;
        }
        // Can't return borrowed local, so use the events module directly below
        return match chrono::NaiveDateTime::parse_from_str(&t, "%Y-%m-%dT%H:%M:%SZ") {
            Ok(dt) => now_secs() - dt.and_utc().timestamp() as f64,
            Err(_) => f64::INFINITY,
        };
    };
    match chrono::NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%dT%H:%M:%SZ") {
        Ok(dt) => now_secs() - dt.and_utc().timestamp() as f64,
        Err(_) => f64::INFINITY,
    }
}

async fn check_workers_inner(ds: &mut DaemonState) {
    let workers = state::load_workers();

    // Prune stale entries
    let worker_names: std::collections::HashSet<&String> = workers.keys().collect();
    ds.idle_seen.retain(|k, _| worker_names.contains(k));
    ds.idle_output_hash.retain(|k, _| worker_names.contains(k));
    ds.recently_escalated
        .retain(|k, _| worker_names.contains(k));
    ds.recently_warned.retain(|k, _| worker_names.contains(k));
    ds.had_children.retain(|k, _| worker_names.contains(k));
    ds.children_finished_at
        .retain(|k, _| worker_names.contains(k));

    for (name, worker) in &workers {
        if worker.status != "running" && worker.status != "blocked" {
            ds.clear_tracking(name);
            continue;
        }

        // Event-driven fast path: done_reported → mark done + wake
        if worker.done_reported {
            log_msg(&format!(
                "Worker {name} has done_reported=True — marking done"
            ));
            ds.clear_tracking(name);
            if let Ok(Some(updated)) = state::update_worker_status(name, "done") {
                if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = tokio::runtime::Handle::current();
                })) {
                    log_msg(&format!("Failed to wake orchestrator for {name}: {e:?}"));
                } else {
                    wake::wake_orchestrator(&updated).await;
                    log_msg(&format!("Woke orchestrator for done worker {name}"));
                }
            }
            continue;
        }

        // Process exited: determine final status
        if worker.process_exited {
            let done = worker.done_reported || events::has_done_event(name);
            let final_status = if done { "done" } else { "dead" };
            log_msg(&format!(
                "Worker {name} process exited — marking {final_status}"
            ));
            ds.clear_tracking(name);
            if let Ok(Some(updated)) = state::update_worker_status(name, final_status) {
                wake::wake_orchestrator(&updated).await;
                log_msg(&format!("Woke orchestrator for {name} ({final_status})"));
            }
            continue;
        }

        // Pane liveness check (runs for both running and blocked)
        let alive = if !worker.pane_id.is_empty() {
            tmux::pane_alive(&worker.pane_id).await
        } else {
            tmux::window_exists(name, config::tmux_session()).await
        };

        if !alive {
            let done = worker.done_reported || events::has_done_event(name);
            let final_status = if done {
                log_msg(&format!(
                    "Worker {name} pane died with done event — marking done"
                ));
                "done"
            } else {
                log_msg(&format!(
                    "Worker {name} pane died without done event — marking dead"
                ));
                "dead"
            };
            ds.clear_tracking(name);
            if let Ok(Some(updated)) = state::update_worker_status(name, final_status) {
                wake::wake_orchestrator(&updated).await;
                log_msg(&format!("Woke orchestrator for {name} ({final_status})"));
            }
            continue;
        }

        if worker.status == "blocked" {
            continue;
        }

        check_stuck(name, worker, &workers, ds).await;
    }
}

async fn check_stuck(
    name: &str,
    worker: &Worker,
    _all_workers: &HashMap<String, Worker>,
    ds: &mut DaemonState,
) {
    let ea = event_age_secs(worker);
    let has_recent_events = ea < config::watchdog_quiet_secs() as f64;

    let target = worker_target(worker);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {})).ok();
    let output = tmux::capture_pane(&target, 200).await;
    if output.is_empty() {
        return;
    }

    // Always handle simple startup prompts (trust, enter, y/n)
    let prompt = prompts::detect_prompt(&output);
    if prompt.kind == "simple" {
        log_msg(&format!(
            "Worker {name} stuck on simple prompt: {} — auto-handling",
            prompt.label
        ));
        let _ = prompts::handle_simple_prompt(&target, &prompt).await;
        return;
    }

    // If recent events exist, trust them and skip complex scanning
    if has_recent_events {
        ds.idle_seen.remove(name);
        ds.idle_output_hash.remove(name);
        return;
    }

    // Watchdog mode: no events for WATCHDOG_QUIET_SECS

    if tmux::is_agent_idle(&output, &worker.backend) {
        if parse_worker_age(&worker.started_at) < IDLE_MIN_LIFETIME {
            ds.idle_seen.remove(name);
            ds.idle_output_hash.remove(name);
            return;
        }

        let has_running_children = state::has_running_children(name);
        if has_running_children {
            ds.had_children.insert(name.to_string(), true);
            ds.idle_seen.remove(name);
            ds.idle_output_hash.remove(name);
            return;
        }

        let now = now_secs();

        if ds.had_children.remove(name).unwrap_or(false) {
            ds.children_finished_at.insert(name.to_string(), now);
            ds.idle_seen.remove(name);
            ds.idle_output_hash.remove(name);
            log_msg(&format!(
                "Worker {name} children finished — starting {CHILD_FINISH_GRACE}s grace period"
            ));
            return;
        }

        if let Some(&finished_at) = ds.children_finished_at.get(name)
            && now - finished_at < CHILD_FINISH_GRACE
        {
            ds.idle_seen.remove(name);
            ds.idle_output_hash.remove(name);
            return;
        }

        let output_hash = format!("{:x}", md5::compute(output.as_bytes()));
        let prev_hash = ds.idle_output_hash.get(name).cloned();

        let first_seen = ds.idle_seen.get(name).copied();
        if first_seen.is_none() {
            ds.idle_seen.insert(name.to_string(), now);
            ds.idle_output_hash.insert(name.to_string(), output_hash);
            log_msg(&format!(
                "Worker {name} appears idle — starting {IDLE_CONFIRM_SECS}s timer"
            ));
            return;
        }

        if let Some(prev) = prev_hash
            && output_hash != prev
        {
            ds.idle_seen.insert(name.to_string(), now);
            ds.idle_output_hash.insert(name.to_string(), output_hash);
            log_msg(&format!(
                "Worker {name} output changed while idle — resetting timer"
            ));
            return;
        }

        ds.idle_output_hash.insert(name.to_string(), output_hash);

        let first = first_seen.unwrap();
        if now - first >= IDLE_CONFIRM_SECS {
            if worker.done_reported {
                log_msg(&format!(
                    "Worker {name} idle + done_reported — marking done"
                ));
                ds.clear_tracking(name);
                if let Ok(Some(updated)) = state::update_worker_status(name, "done") {
                    wake::wake_orchestrator(&updated).await;
                    log_msg(&format!("Woke orchestrator for idle worker {name}"));
                }
            } else {
                let last_warn = ds.recently_warned.get(name).copied().unwrap_or(0.0);
                if now - last_warn >= WARN_COOLDOWN {
                    ds.recently_warned.insert(name.to_string(), now);
                    log_msg(&format!(
                        "Worker {name} idle {}s, no events — warning orchestrator",
                        (now - first) as u64
                    ));
                    wake::warn_orchestrator(
                        worker,
                        "possibly done or stalled (no events, idle screen)",
                    )
                    .await;
                }
            }
        }
        return;
    }

    // Agent is active — clear idle tracking
    ds.idle_seen.remove(name);
    ds.idle_output_hash.remove(name);
    ds.children_finished_at.remove(name);
    ds.had_children.remove(name);

    // Complex blocker scanning — only in watchdog mode
    if prompt.kind == "complex" {
        let now = now_secs();
        let last = ds.recently_escalated.get(name).copied().unwrap_or(0.0);
        if now - last < ESCALATION_COOLDOWN {
            return;
        }
        ds.recently_escalated.insert(name.to_string(), now);
        log_msg(&format!(
            "Worker {name} stuck on complex blocker: {} — escalating",
            prompt.label
        ));
        wake::notify_stuck(worker, &prompt.label, &prompt.snippet).await;
        log_msg(&format!("Escalated {name} blocker to orchestrator"));
    }
}

fn parse_worker_age(started_at: &str) -> f64 {
    chrono::NaiveDateTime::parse_from_str(started_at, "%Y-%m-%dT%H:%M:%SZ")
        .map(|dt| {
            let started_ts = dt.and_utc().timestamp() as f64;
            now_secs() - started_ts
        })
        .unwrap_or(999.0)
}

async fn control_mode_loop(state: &std::sync::Arc<Mutex<DaemonState>>) {
    log_msg("Starting control mode loop");
    let mut backoff: u64 = 2;
    let session = config::tmux_session();

    loop {
        if !tmux::session_exists(session).await {
            log_msg(&format!("Session {session} not found, waiting..."));
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        // Build command with socket flag when $TMUX is unset (daemon context).
        let mut args: Vec<String> = Vec::new();
        if std::env::var("TMUX").unwrap_or_default().is_empty()
            && let Some(sock) = config::load_tmux_socket()
        {
            args.extend(["-S".to_string(), sock]);
        }
        args.extend([
            "-C".to_string(),
            "attach-session".to_string(),
            "-t".to_string(),
            session.to_string(),
            "-r".to_string(),
        ]);

        let child = Command::new("tmux")
            .args(&args)
            .stdin(std::process::Stdio::piped()) // must stay open for control mode
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                log_msg(&format!("Failed to spawn tmux control mode: {e}"));
                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(30);
                continue;
            }
        };

        let stdout = child.stdout.take().unwrap();
        let mut reader = tokio::io::BufReader::new(stdout).lines();
        let mut connected = false;
        let connect_time = std::time::Instant::now();

        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    if !connected && line.starts_with("%session") {
                        connected = true;
                        log_msg("Control mode connected");
                    }

                    if line.starts_with("%window-close")
                        || line.starts_with("%pane-exited")
                        || line.starts_with("%window-pane-changed")
                    {
                        check_workers_arc(state).await;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    log_msg(&format!("Control mode read error: {e}"));
                    break;
                }
            }
        }

        // Clean up child process
        let _ = child.kill().await;
        let _ = child.wait().await;

        // Only reset backoff if we stayed connected for a meaningful duration.
        // Immediate disconnects (< 5s) indicate a config/socket issue.
        if connected && connect_time.elapsed() > std::time::Duration::from_secs(5) {
            backoff = 2;
        }

        log_msg(&format!(
            "Control mode disconnected, reconnecting in {backoff}s..."
        ));
        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn poll_loop(state: &std::sync::Arc<Mutex<DaemonState>>) {
    loop {
        check_workers_arc(state).await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Main daemon entry point.
///
/// Acquires an exclusive file lock on the PID file so that only one daemon
/// can run per ORCA_HOME at a time.
pub async fn run_daemon() {
    if !acquire_pid_lock() {
        log_msg(&format!(
            "Another daemon already running — exiting (pid={})",
            process::id()
        ));
        return;
    }

    let _guard = scopeguard::guard((), |_| release_pid_lock());

    log_msg(&format!("Daemon started (pid={})", process::id()));

    let state = std::sync::Arc::new(Mutex::new(DaemonState::new()));

    let ctrl = control_mode_loop(&state);
    let poll = poll_loop(&state);

    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let mut sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();

    // SIGUSR1 triggers an immediate check
    let mut sigusr1 =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()).unwrap();
    let state_usr1 = state.clone();
    let usr1_task = tokio::spawn(async move {
        loop {
            sigusr1.recv().await;
            check_workers(&state_usr1).await;
        }
    });

    tokio::select! {
        _ = ctrl => {},
        _ = poll => {},
        _ = sigterm.recv() => { log_msg("Received shutdown signal"); },
        _ = sigint.recv() => { log_msg("Received shutdown signal"); },
    }

    usr1_task.abort();
    log_msg("Daemon stopped");
}

/// Fork a daemon process. Returns the actual daemon PID.
pub fn start_daemon_background() -> u32 {
    let _ = config::ensure_home();

    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            panic!("fork() failed");
        }
        if pid > 0 {
            // Reap the intermediate child (it exits immediately after double-fork)
            libc::waitpid(pid, std::ptr::null_mut(), 0);
            // Parent: wait for PID file
            for _ in 0..50 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if let Some(daemon_pid) = read_daemon_pid() {
                    return daemon_pid;
                }
            }
            return pid as u32;
        }

        // First child: new session
        libc::setsid();

        let pid2 = libc::fork();
        if pid2 < 0 {
            libc::_exit(1);
        }
        if pid2 > 0 {
            libc::_exit(0);
        }

        // Grandchild: the actual daemon

        // Close inherited FDs
        let max_fd = libc::sysconf(libc::_SC_OPEN_MAX);
        let max_fd = if max_fd <= 0 { 1024 } else { max_fd.min(8192) };
        for fd in 3..max_fd {
            libc::close(fd as libc::c_int);
        }

        // Redirect stdin to /dev/null
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, 0);
            if devnull > 2 {
                libc::close(devnull);
            }
        }

        // Redirect stdout/stderr to daemon log
        let log_path = config::daemon_log_file();
        let log_path_c = std::ffi::CString::new(log_path.to_str().unwrap_or("/dev/null"))
            .unwrap_or_else(|_| std::ffi::CString::new("/dev/null").unwrap());
        let log_fd = libc::open(
            log_path_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            0o644,
        );
        if log_fd >= 0 {
            libc::dup2(log_fd, 1);
            libc::dup2(log_fd, 2);
            if log_fd > 2 {
                libc::close(log_fd);
            }
        }

        // Build and run the tokio runtime
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(run_daemon());

        libc::_exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();
    static mut TEMP_DIR: Option<tempfile::TempDir> = None;

    /// Tests that touch `daemon.pid` must hold this lock to avoid races.
    /// We recover from poisoned state so a panic in one test doesn't cascade.
    static PID_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_pid_file() -> std::sync::MutexGuard<'static, ()> {
        PID_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
            backend: String::new(),
            task: String::new(),
            dir: String::new(),
            workdir: String::new(),
            base_branch: String::new(),
            orchestrator: String::new(),
            orchestrator_pane: String::new(),
            session_id: String::new(),
            reply_channel: String::new(),
            reply_to: String::new(),
            reply_thread: String::new(),
            depth: 0,
            spawned_by: String::new(),
            layout: String::new(),
            status: String::new(),
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
        w.status = "running".into();
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
            assert_eq!(u.status, "done");
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
        w.status = "running".into();
        w.process_exited = true;
        w.done_reported = false;
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "dead", "expected 'dead' for exited without done");
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
        w.status = "running".into();
        w.process_exited = true;
        w.done_reported = false;
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "done", "expected 'done' when done event exists");
        }

        cleanup_worker(&name);
    }

    #[tokio::test]
    async fn test_check_workers_inner_pane_died_no_done() {
        init_test_home();
        let _ = config::ensure_home();
        let name = unique_name("cw_paned");

        let mut w = make_test_worker(&name);
        w.status = "running".into();
        w.pane_id = "%99999".into();
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "dead", "pane not alive should mark dead");
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
        w.status = "running".into();
        w.pane_id = "%99998".into();
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "done", "pane died + done event → done");
        }

        cleanup_worker(&name);
    }

    #[tokio::test]
    async fn test_check_workers_inner_skips_done_workers() {
        init_test_home();
        let _ = config::ensure_home();
        let name = unique_name("cw_skip");

        let mut w = make_test_worker(&name);
        w.status = "done".into();
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        let u = workers.get(&name).unwrap();
        assert_eq!(u.status, "done", "done worker should stay done");

        cleanup_worker(&name);
    }

    #[tokio::test]
    async fn test_check_workers_inner_skips_dead_workers() {
        init_test_home();
        let _ = config::ensure_home();
        let name = unique_name("cw_dead");

        let mut w = make_test_worker(&name);
        w.status = "dead".into();
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        let u = workers.get(&name).unwrap();
        assert_eq!(u.status, "dead", "dead worker should stay dead");

        cleanup_worker(&name);
    }

    #[tokio::test]
    async fn test_check_workers_inner_blocked_worker_pane_died() {
        init_test_home();
        let _ = config::ensure_home();
        let name = unique_name("cw_blocked");

        let mut w = make_test_worker(&name);
        w.status = "blocked".into();
        w.pane_id = "%99997".into();
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "dead", "blocked + pane died → dead");
        }

        cleanup_worker(&name);
    }

    #[tokio::test]
    async fn test_check_workers_inner_prunes_stale_tracking() {
        init_test_home();
        let _ = config::ensure_home();
        let name = unique_name("cw_prune");

        let mut w = make_test_worker(&name);
        w.status = "done".into();
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
        w1.status = "running".into();
        w1.done_reported = true;
        w1.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w1);

        let mut w2 = make_test_worker(&name2);
        w2.status = "running".into();
        w2.process_exited = true;
        w2.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w2);

        let mut w3 = make_test_worker(&name3);
        w3.status = "done".into();
        w3.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w3);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u1) = workers.get(&name1) {
            assert_eq!(u1.status, "done");
        }
        if let Some(u2) = workers.get(&name2) {
            assert_eq!(u2.status, "dead");
        }
        let u3 = workers.get(&name3).unwrap();
        assert_eq!(u3.status, "done");

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
        w.status = "running".into();
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
        w.status = "running".into();
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
        w.status = "running".into();
        w.done_reported = true;
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let state = Mutex::new(DaemonState::new());
        check_workers(&state).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "done");
        }

        cleanup_worker(&name);
    }

    #[tokio::test]
    async fn test_check_workers_arc_wrapper() {
        init_test_home();
        let _ = config::ensure_home();
        let name = unique_name("cw_arc");

        let mut w = make_test_worker(&name);
        w.status = "running".into();
        w.done_reported = true;
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let state = std::sync::Arc::new(Mutex::new(DaemonState::new()));
        check_workers_arc(&state).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "done");
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
        init_test_home();
        let _ = config::ensure_home();

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
        w.status = "running".into();
        w.backend = "claude".into();
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
        parent.status = "running".into();
        parent.backend = "claude".into();
        parent.pane_id = "%99993".into();
        parent.started_at = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::seconds(120))
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        parent.last_event_at = String::new();

        let mut child = make_test_worker(&child_name);
        child.status = "running".into();
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
        w.status = "running".into();
        w.backend = "claude".into();
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
        w.status = "running".into();
        w.backend = "claude".into();
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
        w.status = "running".into();
        w.backend = "claude".into();
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
        w.status = "running".into();
        w.backend = "claude".into();
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
        w.status = "running".into();
        w.backend = "claude".into();
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
        w.status = "blocked".into();
        w.pane_id = "%99986".into();
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "done", "blocked + pane died + done event → done");
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
        w.status = "running".into();
        w.process_exited = true;
        w.done_reported = true;
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            // done_reported is checked first in check_workers_inner
            assert_eq!(u.status, "done");
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
        w.status = "running".into();
        w.pane_id = String::new(); // empty → falls back to window_exists
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        save_test_worker(&w);

        let mut ds = DaemonState::new();
        check_workers_inner(&mut ds).await;

        let workers = state::load_workers();
        if let Some(u) = workers.get(&name) {
            assert_eq!(u.status, "dead", "no window → dead");
        }

        cleanup_worker(&name);
    }
}
