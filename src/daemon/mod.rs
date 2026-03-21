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
use crate::types::WorkerStatus;
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
    let path = config::daemon_log_file();
    if std::fs::create_dir_all(config::orca_home()).is_err() {
        return;
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
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
        // Only unlink the pidfile when we held the daemon lock; otherwise a no-op
        // release (e.g. tests) must not delete the file — parallel tests share one path.
        let _ = fs::remove_file(config::daemon_pid_file());
    }
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
        // Skip L0 orchestrator entries — they are bookkeeping, not real workers.
        // Monitoring them causes false idle/done detection on the orchestrator's pane.
        if worker.depth == 0 && worker.spawned_by.is_empty() {
            continue;
        }

        if !worker.status.is_active() {
            ds.clear_tracking(name);
            continue;
        }

        // Event-driven fast path: done_reported → mark done + wake
        if worker.done_reported {
            log_msg(&format!(
                "Worker {name} has done_reported=True — marking done"
            ));
            ds.clear_tracking(name);
            if let Ok(Some(updated)) = state::update_worker_status(name, WorkerStatus::Done) {
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
            let final_status = if done {
                WorkerStatus::Done
            } else {
                WorkerStatus::Dead
            };
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
                WorkerStatus::Done
            } else {
                log_msg(&format!(
                    "Worker {name} pane died without done event — marking dead"
                ));
                WorkerStatus::Dead
            };
            ds.clear_tracking(name);
            if let Ok(Some(updated)) = state::update_worker_status(name, final_status) {
                wake::wake_orchestrator(&updated).await;
                log_msg(&format!("Woke orchestrator for {name} ({final_status})"));
            }
            continue;
        }

        if worker.status == WorkerStatus::Blocked {
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

    if tmux::is_agent_idle(&output, &worker.backend.to_string()) {
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
                if let Ok(Some(updated)) = state::update_worker_status(name, WorkerStatus::Done) {
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

/// Whether a tmux session is reachable (via `$TMUX` or saved socket).
/// Without tmux the daemon cannot monitor panes or deliver notifications.
pub fn can_reach_tmux() -> bool {
    if !std::env::var("TMUX").unwrap_or_default().is_empty() {
        return true;
    }
    config::load_tmux_socket().is_some()
}

/// Fork a daemon process. Returns the actual daemon PID.
/// Returns 0 without forking when tmux is unreachable.
pub fn start_daemon_background() -> u32 {
    if !can_reach_tmux() {
        return 0;
    }
    let _ = config::ensure_home();

    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            eprintln!("orca: fork() failed");
            return 0;
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

        // Change to a stable directory so subprocesses don't crash
        // when the inherited cwd is a worktree that gets deleted later.
        if let Some(home) = dirs::home_dir()
            && let Ok(home_c) = std::ffi::CString::new(home.to_string_lossy().as_ref())
        {
            libc::chdir(home_c.as_ptr());
        }

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
mod tests;
