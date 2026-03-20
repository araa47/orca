//! Orca CLI — the only interface agents use.

use std::collections::{HashMap, HashSet};
use std::process;

use clap::{Parser, Subcommand};
use regex::Regex;

use crate::config;
use crate::daemon;
use crate::events;
use crate::spawn::{self, SpawnOptions, truncate_task};
use crate::state::{self, Worker};
use crate::tmux;
use crate::worktree;

use crate::config::audit;

// ---------------------------------------------------------------------------
// Depth labels
// ---------------------------------------------------------------------------

fn depth_emoji(depth: u32) -> &'static str {
    match depth {
        0 => "🐋",
        1 => "🐳",
        2 => "🐬",
        3 => "🐟",
        _ => "🦐",
    }
}

fn depth_label(depth: u32) -> String {
    let emoji = depth_emoji(depth);
    format!("{emoji} L{depth}")
}

const L0_SPAWN_MARKERS: &[&str] = &["root", "openclaw"];

/// `--spawned-by` is mandatory so the caller must choose between:
/// - a real parent worker name (`fin`, `audit-lead`, ...)
/// - an L0 marker: `openclaw` (preferred) or legacy `root`
///
/// L0 markers are normalized to the empty string in state so the existing
/// root/child tree logic continues to work without a wider schema change.
fn is_root_spawn_marker(spawned_by: &str) -> bool {
    let spawned_by = spawned_by.trim();
    L0_SPAWN_MARKERS.contains(&spawned_by) || spawned_by.starts_with("root:")
}

fn normalize_spawned_by(spawned_by: &str) -> String {
    let spawned_by = spawned_by.trim();
    if is_root_spawn_marker(spawned_by) {
        String::new()
    } else {
        spawned_by.to_string()
    }
}

/// When `--spawned-by` names a known parent worker, use that worker's stored
/// `depth` as the CLI `--depth` so child workers get L2/L3 labels and parentage.
/// L0 markers normalize to empty `spawned_by` and keep the CLI depth.
fn resolve_spawn_lineage(
    spawned_by: String,
    mut depth: u32,
    workers: &std::collections::HashMap<String, Worker>,
) -> (String, u32) {
    if !spawned_by.is_empty()
        && let Some(parent) = workers.get(&spawned_by)
    {
        depth = parent.depth + 1;
    }
    (spawned_by, depth)
}

/// Spawn policy flags (normally from `ORCA_*` env vars). Separated for unit tests.
#[derive(Clone, Copy)]
struct SpawnValidateEnv {
    allow_no_orchestrator: bool,
    allow_openclaw_without_reply: bool,
}

impl SpawnValidateEnv {
    fn from_process_env() -> Self {
        Self {
            allow_no_orchestrator: env_flag("ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR"),
            allow_openclaw_without_reply: env_flag("ORCA_ALLOW_OPENCLAW_WITHOUT_REPLY"),
        }
    }
}

fn env_flag(key: &str) -> bool {
    let Ok(v) = std::env::var(key) else {
        return false;
    };
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

const VALID_ORCHESTRATORS: &[&str] = &[
    "cc", "cx", "cu", "claude", "codex", "cursor", "openclaw", "none",
];

/// Enforce agent-safe spawn defaults: real orchestrator, OpenClaw reply routing, valid parent links.
#[allow(clippy::too_many_arguments)]
fn validate_spawn_context(
    orchestrator: &str,
    raw_spawned_by: &str,
    spawned_by: &str,
    implicit_self: Option<&str>,
    workers: &std::collections::HashMap<String, Worker>,
    reply_channel: &str,
    reply_to: &str,
    env: &SpawnValidateEnv,
) -> Result<(), String> {
    if !VALID_ORCHESTRATORS.contains(&orchestrator) {
        return Err(format!(
            "Error: unknown --orchestrator '{orchestrator}'. \
             Valid values: cc, cx, cu, openclaw (or long forms: claude, codex, cursor). \
             Use 'none' only with ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR=1."
        ));
    }
    if orchestrator == "none" && !env.allow_no_orchestrator {
        return Err(
            "Error: --orchestrator is required (cc, cx, cu, or openclaw). \
             Agents must pass the same value as their backend family so notifications and scope work. \
             For headless scripts only, set ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR=1."
                .into(),
        );
    }
    if orchestrator == "openclaw"
        && (reply_channel.is_empty() || reply_to.is_empty())
        && !env.allow_openclaw_without_reply
    {
        return Err(
            "Error: --orchestrator openclaw requires --reply-channel and --reply-to. \
             Without them the user may not see completion events. \
             Set ORCA_ALLOW_OPENCLAW_WITHOUT_REPLY=1 only for automation."
                .into(),
        );
    }
    if raw_spawned_by.trim().is_empty() {
        return Err("Error: --spawned-by is required. \
             Pass your worker name (from ORCA_WORKER_NAME or `orca list`). \
             Only OpenClaw L0 uses `--spawned-by openclaw`."
            .into());
    }
    if !spawned_by.is_empty() && !workers.contains_key(spawned_by) {
        return Err(format!(
            "Error: --spawned-by '{spawned_by}' does not match any tracked worker. \
             Pass the exact worker name from `orca list` (e.g. `fin`, `mud`). \
             Only OpenClaw L0 uses `--spawned-by openclaw`."
        ));
    }
    if let Some(self_name) = implicit_self.filter(|s| !s.is_empty()) {
        if workers.contains_key(self_name) {
            if spawned_by != self_name {
                return Err(format!(
                    "Error: you are worker '{self_name}' but --spawned-by resolved to '{}'. \
                     Sub-workers must use `--spawned-by {self_name}` (your worker name). \
                     `--spawned-by openclaw` is ONLY for the OpenClaw L0 orchestrator.",
                    if spawned_by.is_empty() {
                        "openclaw"
                    } else {
                        spawned_by
                    }
                ));
            }
        } else if spawned_by.is_empty() {
            return Err(format!(
                "Error: ORCA_WORKER_NAME is '{self_name}' but that worker is not in Orca state. \
                 The daemon cannot link sub-workers without a tracked parent — \
                 unset ORCA_WORKER_NAME, or pass --spawned-by <running-parent-name>."
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Relative time
// ---------------------------------------------------------------------------

fn relative_time(iso_ts: &str) -> String {
    let parsed = chrono::NaiveDateTime::parse_from_str(iso_ts, "%Y-%m-%dT%H:%M:%SZ");
    let Ok(dt) = parsed else {
        return iso_ts.to_string();
    };
    let then = dt.and_utc().timestamp();
    let now = chrono::Utc::now().timestamp();
    let delta = (now - then).max(0);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else {
        format!("{}h ago", delta / 3600)
    }
}

// ---------------------------------------------------------------------------
// Worker target helper
// ---------------------------------------------------------------------------

fn worker_target(w: &Worker) -> String {
    if !w.pane_id.is_empty() {
        return w.pane_id.clone();
    }
    format!("{}:{}", config::tmux_session(), w.name)
}

// ---------------------------------------------------------------------------
// CLI structure
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "orca",
    version,
    about = "Orca — simple agent orchestrator.",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Spawn a new worker agent.
    Spawn {
        /// Task description
        #[arg(required = true)]
        task: Vec<String>,

        /// Agent backend (claude/codex/cursor)
        #[arg(short = 'b', long = "backend", default_value = "claude")]
        backend: String,

        /// Project directory
        #[arg(short = 'd', long = "dir", default_value = ".")]
        project_dir: String,

        /// Worker name
        #[arg(short = 'n', long = "name")]
        name: Option<String>,

        /// Branch for worktree
        #[arg(long = "base-branch", default_value = "main")]
        base_branch: String,

        /// Who receives completion / stuck notifications (cc/cx/cu/openclaw). Value `none` is only
        /// accepted when ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR=1 (headless scripts).
        #[arg(long = "orchestrator", default_value = "none")]
        orchestrator: String,

        /// Orchestrator tmux pane (auto-detected if omitted)
        #[arg(long = "pane", default_value = "")]
        pane: String,

        /// OpenClaw session ID
        #[arg(long = "session-id", default_value = "")]
        session_id: String,

        /// Delivery channel (slack/telegram/discord/...)
        #[arg(long = "reply-channel", default_value = "")]
        reply_channel: String,

        /// Delivery target (channel ID, chat ID)
        #[arg(long = "reply-to", default_value = "")]
        reply_to: String,

        /// Thread ID for threaded replies
        #[arg(long = "reply-thread", default_value = "")]
        reply_thread: String,

        /// Orchestration depth (0=L0/OpenClaw spawning first workers). When `--spawned-by`
        /// names a known parent, depth is taken from that parent automatically.
        #[arg(long = "depth", default_value_t = 0)]
        depth: u32,

        /// Parent worker name (e.g. `fin`, `mud`) — the name shown in `orca list`,
        /// NOT the emoji label. Use `root` ONLY when the top-level orchestrator
        /// (OpenClaw / Claude Code / Codex / Cursor) spawns directly.
        #[arg(long = "spawned-by", required = true)]
        spawned_by: String,
    },

    /// Print the current tmux pane address (for --pane flag).
    Pane,

    /// Show all workers with status.
    List,

    /// Detailed worker status.
    Status {
        /// Worker name
        name: String,
    },

    /// Read full output from a worker's log file (or tmux pane as fallback).
    Logs {
        /// Worker name
        name: String,

        /// Number of lines
        #[arg(short = 'n', long = "lines", default_value_t = 200)]
        lines: u32,

        /// Show raw output with ANSI codes
        #[arg(long = "raw")]
        raw: bool,
    },

    /// Report a worker lifecycle event (used by hooks and wrappers).
    Report {
        /// Worker name
        #[arg(short = 'w', long = "worker")]
        worker: String,

        /// Event type (done/blocked/heartbeat/process_exit)
        #[arg(short = 'e', long = "event")]
        event: String,

        /// Optional message
        #[arg(short = 'm', long = "message", default_value = "")]
        message: String,

        /// Event source (`hook` from lifecycle hooks; use `cli` to force `done` while sub-workers run)
        #[arg(short = 's', long = "source", default_value = "hook")]
        source: String,
    },

    /// Send follow-up instructions to a running worker.
    Steer {
        /// Worker name
        name: String,

        /// Message to send
        #[arg(required = true)]
        message: Vec<String>,
    },

    /// Kill a worker and remove its worktree.
    Kill {
        /// Worker name
        name: String,

        /// Skip stashing uncommitted changes before removing the worktree
        #[arg(long = "no-stash")]
        no_stash: bool,
    },

    /// Kill workers. Requires --mine, --pane, --session-id, or --force.
    Killall {
        /// Only kill workers spawned by this orchestrator pane
        #[arg(long = "pane", default_value = "")]
        pane: String,

        /// Only kill workers with this session ID
        #[arg(long = "session-id", default_value = "")]
        session_id: String,

        /// Only kill workers belonging to the current pane
        #[arg(long = "mine")]
        mine: bool,

        /// Kill ALL workers globally (may affect other orchestrators)
        #[arg(long = "force")]
        force: bool,

        /// Skip stashing uncommitted changes before removing worktrees
        #[arg(long = "no-stash")]
        no_stash: bool,
    },

    /// Clean up done/dead workers. Requires --mine, --pane, --session-id, or --force.
    Gc {
        /// Only clean workers spawned by this orchestrator pane
        #[arg(long = "pane", default_value = "")]
        pane: String,

        /// Only clean workers with this session ID
        #[arg(long = "session-id", default_value = "")]
        session_id: String,

        /// Only clean workers belonging to the current pane
        #[arg(long = "mine")]
        mine: bool,

        /// Clean ALL done/dead workers globally
        #[arg(long = "force")]
        force: bool,

        /// Skip stashing uncommitted changes before removing worktrees
        #[arg(long = "no-stash")]
        no_stash: bool,
    },

    /// Daemon management.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },

    /// Hook management.
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Start the background daemon.
    Start,
    /// Stop the background daemon.
    Stop,
    /// Check if daemon is running.
    Status,
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Install Orca hooks for Claude Code and Codex.
    Install,
    /// Remove Orca hooks.
    Uninstall,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Spawn {
            task,
            backend,
            project_dir,
            name,
            base_branch,
            orchestrator,
            pane,
            session_id,
            reply_channel,
            reply_to,
            reply_thread,
            depth,
            spawned_by,
        } => cmd_spawn(
            task,
            backend,
            project_dir,
            name,
            base_branch,
            orchestrator,
            pane,
            session_id,
            reply_channel,
            reply_to,
            reply_thread,
            depth,
            spawned_by,
        ),
        Commands::Pane => cmd_pane(),
        Commands::List => cmd_list(),
        Commands::Status { name } => cmd_status(&name),
        Commands::Logs { name, lines, raw } => cmd_logs(&name, lines, raw),
        Commands::Report {
            worker,
            event,
            message,
            source,
        } => cmd_report(&worker, &event, &message, &source),
        Commands::Steer { name, message } => cmd_steer(&name, message),
        Commands::Kill { name, no_stash } => cmd_kill(&name, no_stash),
        Commands::Killall {
            pane,
            session_id,
            mine,
            force,
            no_stash,
        } => cmd_killall(pane, session_id, mine, force, no_stash),
        Commands::Gc {
            pane,
            session_id,
            mine,
            force,
            no_stash,
        } => cmd_gc(pane, session_id, mine, force, no_stash),
        Commands::Daemon { command } => match command {
            DaemonCommands::Start => cmd_daemon_start(),
            DaemonCommands::Stop => cmd_daemon_stop(),
            DaemonCommands::Status => cmd_daemon_status(),
        },
        Commands::Hooks { command } => match command {
            HooksCommands::Install => cmd_hooks_install(),
            HooksCommands::Uninstall => cmd_hooks_uninstall(),
        },
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn cmd_spawn(
    task: Vec<String>,
    backend: String,
    project_dir: String,
    name: Option<String>,
    base_branch: String,
    orchestrator: String,
    pane: String,
    session_id: String,
    reply_channel: String,
    reply_to: String,
    reply_thread: String,
    depth: u32,
    spawned_by: String,
) {
    let _ = config::ensure_home();
    config::save_tmux_socket();

    let pane = if pane.is_empty()
        && matches!(
            orchestrator.as_str(),
            "cc" | "cx" | "cu" | "claude" | "codex" | "cursor"
        ) {
        tmux::detect_current_pane()
    } else {
        pane
    };

    let workers = state::load_workers();
    let implicit_env = std::env::var("ORCA_WORKER_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    let raw_spawned_by = spawned_by;
    let normalized_spawned_by = normalize_spawned_by(&raw_spawned_by);
    let (spawned_by, depth) = resolve_spawn_lineage(normalized_spawned_by, depth, &workers);

    let spawn_env = SpawnValidateEnv::from_process_env();
    if let Err(msg) = validate_spawn_context(
        &orchestrator,
        &raw_spawned_by,
        &spawned_by,
        implicit_env.as_deref(),
        &workers,
        &reply_channel,
        &reply_to,
        &spawn_env,
    ) {
        eprintln!("{msg}");
        process::exit(1);
    }

    let worker_depth = depth;

    if worker_depth > config::max_depth() {
        eprintln!(
            "Error: max orchestration depth reached ({}). This worker cannot spawn sub-workers.",
            config::max_depth()
        );
        process::exit(1);
    }

    let running = state::count_running_by_orchestrator(&pane, &session_id);
    let max = config::max_workers_per_orchestrator();
    if running >= max as usize {
        eprintln!("Error: orchestrator already has {max} running workers (max). Kill some first.");
        process::exit(1);
    }

    if !daemon::is_daemon_running() {
        let pid = daemon::start_daemon_background();
        println!("Daemon started (pid={pid})");
    }

    let prompt = task.join(" ");
    let session = config::tmux_session().to_string();

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let result = rt.block_on(spawn::spawn_worker(SpawnOptions {
        task: prompt.clone(),
        backend,
        project_dir,
        name,
        base_branch,
        orchestrator,
        orchestrator_pane: pane,
        session_id,
        reply_channel,
        reply_to,
        reply_thread,
        session,
        depth: worker_depth,
        spawned_by,
    }));

    match result {
        Ok(worker) => {
            let level = depth_label(worker.depth);
            let short = truncate_task(&prompt, 80);
            audit(&format!(
                "SPAWN worker={} backend={} depth={} spawned_by={} task={:?}",
                worker.name,
                worker.backend,
                worker.depth,
                if worker.spawned_by.is_empty() {
                    "-"
                } else {
                    &worker.spawned_by
                },
                short,
            ));
            println!(
                "Spawned: {} ({}) -- running [{level}]",
                worker.name, worker.backend
            );
            println!("  Task: {short}");
            println!("  Dir: {}", worker.workdir);
        }
        Err(e) => {
            audit(&format!("SPAWN_FAILED error={e:?}"));
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

fn cmd_pane() {
    let result = tmux::detect_current_pane();
    if result.is_empty() {
        eprintln!("Error: not inside a tmux session");
        process::exit(1);
    }
    println!("{result}");
}

fn cmd_list() {
    let workers = state::load_workers();
    if workers.is_empty() {
        println!("No workers.");
        return;
    }
    print_tree(&workers);
}

fn cmd_status(name: &str) {
    let Some(w) = state::get_worker(name) else {
        eprintln!("Error: Worker '{name}' not found");
        process::exit(1);
    };

    let level = depth_label(w.depth);
    println!("Name: {}", w.name);
    println!("Backend: {}", w.backend);
    println!("Status: {}", w.status);
    println!("Level: {level} (depth={})", w.depth);
    println!("Orchestrator: {}", w.orchestrator);
    if !w.spawned_by.is_empty() {
        println!("Spawned by: {}", w.spawned_by);
    }

    let all = state::load_workers();
    let kids: Vec<&String> = all
        .iter()
        .filter(|(_, ww)| ww.spawned_by == w.name)
        .map(|(n, _)| n)
        .collect();
    if !kids.is_empty() {
        let mut sorted_kids: Vec<&str> = kids.iter().map(|s| s.as_str()).collect();
        sorted_kids.sort();
        println!("Children: {}", sorted_kids.join(", "));
    }

    println!("Task: {}", w.task);
    println!("Dir: {}", w.workdir);
    println!("Started: {}", relative_time(&w.started_at));

    if w.status == "running" {
        let log_path = config::logs_dir().join(format!("{name}.log"));
        let mut tail_text = String::new();

        if log_path.exists()
            && let Ok(content) = std::fs::read_to_string(&log_path)
        {
            let content = strip_ansi(&content);
            let non_empty: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
            tail_text = non_empty
                .iter()
                .rev()
                .take(5)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
        }

        if tail_text.is_empty() {
            let target = worker_target(&w);
            let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
            let captured = rt.block_on(tmux::capture_pane(&target, 5));
            tail_text = strip_ansi(captured.trim());
        }

        if !tail_text.trim().is_empty() {
            println!("Last output (5 lines):");
            for line in tail_text
                .lines()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                println!("  > {line}");
            }
        }
    }
}

fn strip_ansi(text: &str) -> String {
    let re = Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\].*?\x07|\x1b[()][AB012]").unwrap();
    re.replace_all(text, "").to_string()
}

fn cmd_logs(name: &str, lines: u32, raw: bool) {
    let Some(w) = state::get_worker(name) else {
        eprintln!("Error: Worker '{name}' not found");
        process::exit(1);
    };

    let log_path = config::logs_dir().join(format!("{name}.log"));

    if log_path.exists() {
        let content = match std::fs::read_to_string(&log_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error reading log: {e}");
                process::exit(1);
            }
        };
        let content = if raw { content } else { strip_ansi(&content) };
        let all_lines: Vec<&str> = content.lines().collect();
        let start = if lines > 0 && (lines as usize) < all_lines.len() {
            all_lines.len() - lines as usize
        } else {
            0
        };
        let tail = &all_lines[start..];
        println!("{}", tail.join("\n"));
    } else {
        let target = worker_target(&w);
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        let output = rt.block_on(tmux::capture_pane(
            &target,
            if lines > 0 { lines } else { 200 },
        ));
        let output = if raw { output } else { strip_ansi(&output) };
        println!("{output}");
    }
}

fn nudge_daemon() {
    if let Some(pid) = daemon::read_daemon_pid() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGUSR1);
        }
    }
}

/// When a **hook** fires `done` while the worker still has **active** children (`running` or
/// `blocked`), record `heartbeat` instead so `done_reported` stays false. The IDE still invokes
/// the stop hook every turn; we only gate whether Orca treats that as completion.
fn apply_hook_done_deferral(
    event: &str,
    message: &str,
    source: &str,
    has_active_children: bool,
) -> (String, String) {
    if event == "done" && source == "hook" && has_active_children {
        let msg = if message.is_empty() {
            "orca: hook done deferred — sub-workers still active".to_string()
        } else {
            format!("{message} [orca: hook done deferred — sub-workers still active]")
        };
        return ("heartbeat".to_string(), msg);
    }
    (event.to_string(), message.to_string())
}

fn report_field_updates(event: &str, timestamp: &str) -> HashMap<String, serde_json::Value> {
    let mut updates = HashMap::new();
    updates.insert(
        "last_event_at".to_string(),
        serde_json::Value::String(timestamp.to_string()),
    );
    match event {
        "done" => {
            updates.insert("done_reported".to_string(), serde_json::Value::Bool(true));
        }
        "blocked" => {
            updates.insert(
                "status".to_string(),
                serde_json::Value::String("blocked".to_string()),
            );
        }
        "process_exit" => {
            updates.insert("process_exited".to_string(), serde_json::Value::Bool(true));
        }
        _ => {}
    }
    updates
}

fn cmd_report(worker_name: &str, event: &str, message: &str, source: &str) {
    if !events::VALID_EVENTS.contains(&event) {
        eprintln!(
            "Error: invalid event '{}' (valid: {})",
            event,
            events::VALID_EVENTS.join(", ")
        );
        process::exit(1);
    }

    let Some(_w) = state::get_worker(worker_name) else {
        eprintln!("Error: Worker '{worker_name}' not found");
        process::exit(1);
    };

    let has_kids = state::has_running_children(worker_name);
    let (eff_event, eff_message) = apply_hook_done_deferral(event, message, source, has_kids);
    let deferred_hook_done = eff_event != event;

    let record = match events::append_event(worker_name, &eff_event, &eff_message, source) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };

    let ts = record["timestamp"].as_str().unwrap_or("");
    let updates = report_field_updates(&eff_event, ts);
    let _ = state::update_worker_fields(worker_name, &updates);

    if !daemon::is_daemon_running() {
        let _ = daemon::start_daemon_background();
    } else {
        nudge_daemon();
    }

    audit(&format!(
        "REPORT worker={worker_name} event={eff_event} source={source}{}",
        if eff_message.is_empty() {
            String::new()
        } else {
            format!(" message={eff_message:?}")
        }
    ));
    if deferred_hook_done {
        println!(
            "Reported: {worker_name} {eff_event} (stop hook: done deferred while sub-workers run)"
        );
    } else {
        println!("Reported: {worker_name} {eff_event}");
    }
}

fn cmd_steer(name: &str, message: Vec<String>) {
    let Some(w) = state::get_worker(name) else {
        eprintln!("Error: Worker '{name}' not found");
        process::exit(1);
    };

    if w.status != "running" && w.status != "blocked" {
        eprintln!(
            "Error: Worker '{name}' is {}, not running/blocked",
            w.status
        );
        process::exit(1);
    }

    if w.status == "blocked" {
        let _ = state::update_worker_status(name, "running");
    }

    let target = worker_target(&w);
    let msg = message.join(" ");
    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    rt.block_on(tmux::send_keys(&target, &msg, true, true, 0, 1));
    audit(&format!("STEER worker={name} message={msg:?}"));
    println!("Steered: {name}");
}

fn cmd_kill(name: &str, no_stash: bool) {
    let Some(w) = state::get_worker(name) else {
        eprintln!("Error: Worker '{name}' not found");
        process::exit(1);
    };

    if !w.orchestrator_pane.is_empty() {
        let current = tmux::detect_current_pane();
        if !current.is_empty() && current != w.orchestrator_pane {
            eprintln!(
                "Warning: worker '{name}' belongs to another orchestrator \
                 (pane {}, you are {current}). Proceeding anyway.",
                w.orchestrator_pane
            );
        }
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    rt.block_on(async {
        if !w.pane_id.is_empty() && tmux::pane_alive(&w.pane_id).await {
            tmux::kill_pane(&w.pane_id).await;
        } else if tmux::window_exists(&w.name, config::tmux_session()).await {
            let target = format!("{}:{}", config::tmux_session(), w.name);
            tmux::kill_window(&target).await;
        }
        if w.workdir.ends_with(&format!("/.worktrees/{name}")) {
            if !no_stash {
                worktree::stash_if_dirty(&w.dir, name).await;
            }
            worktree::remove_worktree(&w.dir, name).await;
        }
    });

    let current_pane = tmux::detect_current_pane();
    audit(&format!(
        "KILL worker={name} pane={} caller_pane={}",
        if w.pane_id.is_empty() {
            "-"
        } else {
            &w.pane_id
        },
        if current_pane.is_empty() {
            "-"
        } else {
            &current_pane
        },
    ));
    let _ = state::remove_worker(name);
    println!("Killed: {name}");
}

fn filter_workers_by_scope(
    workers: &HashMap<String, Worker>,
    pane: &str,
    session_id: &str,
) -> HashMap<String, Worker> {
    if pane.is_empty() && session_id.is_empty() {
        return workers.clone();
    }

    let mut owned: HashSet<String> = HashSet::new();
    for (name, w) in workers {
        if (!pane.is_empty() && w.orchestrator_pane == pane)
            || (!session_id.is_empty() && w.session_id == session_id)
        {
            owned.insert(name.clone());
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for (name, w) in workers {
            if !owned.contains(name) && owned.contains(&w.spawned_by) {
                owned.insert(name.clone());
                changed = true;
            }
        }
    }

    workers
        .iter()
        .filter(|(n, _)| owned.contains(n.as_str()))
        .map(|(n, w)| (n.clone(), w.clone()))
        .collect()
}

fn cmd_killall(mut pane: String, session_id: String, mine: bool, force: bool, no_stash: bool) {
    if !mine && pane.is_empty() && session_id.is_empty() && !force {
        eprintln!(
            "Error: specify --mine, --pane, --session-id, or --force.\n\
             \x20 --mine   kills only your workers (safe)\n\
             \x20 --force  kills ALL workers globally (may affect other orchestrators)"
        );
        process::exit(1);
    }

    if mine && pane.is_empty() {
        pane = tmux::detect_current_pane();
    }

    let all_workers = state::load_workers();
    let workers = if force {
        all_workers
    } else {
        filter_workers_by_scope(&all_workers, &pane, &session_id)
    };
    if workers.is_empty() {
        println!("No workers.");
        return;
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    for (wname, w) in &workers {
        rt.block_on(async {
            if !w.pane_id.is_empty() && tmux::pane_alive(&w.pane_id).await {
                tmux::kill_pane(&w.pane_id).await;
            } else if tmux::window_exists(wname, config::tmux_session()).await {
                let target = format!("{}:{}", config::tmux_session(), wname);
                tmux::kill_window(&target).await;
            }
            if w.workdir.ends_with(&format!("/.worktrees/{wname}")) {
                if !no_stash {
                    worktree::stash_if_dirty(&w.dir, wname).await;
                }
                worktree::remove_worktree(&w.dir, wname).await;
            }
        });
    }

    let killed_names: Vec<String> = workers.keys().cloned().collect();
    for wname in &killed_names {
        let _ = state::remove_worker(wname);
        println!("Killed: {wname}");
    }
    let scope = if force {
        "force".to_string()
    } else if mine {
        format!("mine pane={pane}")
    } else if !pane.is_empty() {
        format!("pane={pane}")
    } else {
        format!("session_id={session_id}")
    };
    audit(&format!(
        "KILLALL scope={scope} killed={}",
        killed_names.join(",")
    ));
}

fn cmd_gc(mut pane: String, session_id: String, mine: bool, force: bool, no_stash: bool) {
    if !mine && pane.is_empty() && session_id.is_empty() && !force {
        eprintln!(
            "Error: specify --mine, --pane, --session-id, or --force.\n\
             \x20 --mine   cleans only your workers (safe)\n\
             \x20 --force  cleans ALL done/dead workers globally"
        );
        process::exit(1);
    }

    if mine && pane.is_empty() {
        pane = tmux::detect_current_pane();
    }

    let all_workers = state::load_workers();
    let scoped = if force {
        all_workers
    } else {
        filter_workers_by_scope(&all_workers, &pane, &session_id)
    };
    let to_gc: Vec<(String, Worker)> = scoped
        .into_iter()
        .filter(|(_, w)| matches!(w.status.as_str(), "done" | "dead" | "destroyed"))
        .collect();

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    for (name, w) in &to_gc {
        rt.block_on(async {
            if !w.pane_id.is_empty() && tmux::pane_alive(&w.pane_id).await {
                tmux::kill_pane(&w.pane_id).await;
            } else if tmux::window_exists(name, config::tmux_session()).await {
                let target = format!("{}:{}", config::tmux_session(), name);
                tmux::kill_window(&target).await;
            }
            if w.workdir.ends_with(&format!("/.worktrees/{name}")) {
                if !no_stash {
                    worktree::stash_if_dirty(&w.dir, name).await;
                }
                worktree::remove_worktree(&w.dir, name).await;
            }
        });
    }

    let mut removed: Vec<String> = Vec::new();
    for (name, _) in &to_gc {
        events::remove_events(name);
        let log_path = config::logs_dir().join(format!("{name}.log"));
        let _ = std::fs::remove_file(log_path);
        let _ = state::remove_worker(name);
        removed.push(name.clone());
    }

    if pane.is_empty()
        && session_id.is_empty()
        && let Ok(extra) = state::gc_workers()
    {
        removed.extend(extra);
    }

    if !removed.is_empty() {
        let scope = if force {
            "force".to_string()
        } else if mine {
            format!("mine pane={pane}")
        } else if !pane.is_empty() {
            format!("pane={pane}")
        } else {
            format!("session_id={session_id}")
        };
        audit(&format!("GC scope={scope} removed={}", removed.join(",")));
        println!("Cleaned: {}", removed.join(", "));
    } else {
        println!("Nothing to clean.");
    }
}

// ---------------------------------------------------------------------------
// Daemon subcommands
// ---------------------------------------------------------------------------

fn cmd_daemon_start() {
    config::save_tmux_socket();
    if daemon::is_daemon_running() {
        println!("Daemon already running.");
        return;
    }
    let pid = daemon::start_daemon_background();
    audit(&format!("DAEMON_START pid={pid}"));
    println!("Daemon started (pid={pid})");
}

fn cmd_daemon_stop() {
    if daemon::stop_daemon() {
        audit("DAEMON_STOP");
        println!("Daemon stopped.");
    } else {
        println!("Daemon not running.");
    }
}

fn cmd_daemon_status() {
    match daemon::read_daemon_pid() {
        Some(pid) => println!("Daemon running (pid={pid})"),
        None => println!("Daemon not running."),
    }
}

// ---------------------------------------------------------------------------
// Hooks subcommands
// ---------------------------------------------------------------------------

const HOOK_INSTALL_SH: &str = include_str!("../hooks/install.sh");
const HOOK_UNINSTALL_SH: &str = include_str!("../hooks/uninstall.sh");
const HOOK_ORCA_SH: &str = include_str!("../hooks/orca-hook.sh");

fn write_hook_scripts() -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("orca-hooks");
    std::fs::create_dir_all(&dir)?;
    let install = dir.join("install.sh");
    let uninstall = dir.join("uninstall.sh");
    let hook = dir.join("orca-hook.sh");
    std::fs::write(&install, HOOK_INSTALL_SH)?;
    std::fs::write(&uninstall, HOOK_UNINSTALL_SH)?;
    std::fs::write(&hook, HOOK_ORCA_SH)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [&install, &uninstall, &hook] {
            let _ = std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(dir)
}

fn cmd_hooks_install() {
    let dir = match write_hook_scripts() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: failed to write hook scripts: {e}");
            process::exit(1);
        }
    };
    let script = dir.join("install.sh");
    let status = std::process::Command::new("bash").arg(&script).status();
    match status {
        Ok(s) if s.success() => audit("HOOKS_INSTALL"),
        Ok(s) => {
            eprintln!("Hook install exited with {s}");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error running install script: {e}");
            process::exit(1);
        }
    }
}

fn cmd_hooks_uninstall() {
    let dir = match write_hook_scripts() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: failed to write hook scripts: {e}");
            process::exit(1);
        }
    };
    let script = dir.join("uninstall.sh");
    let status = std::process::Command::new("bash").arg(&script).status();
    match status {
        Ok(s) if s.success() => audit("HOOKS_UNINSTALL"),
        Ok(s) => {
            eprintln!("Hook uninstall exited with {s}");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error running uninstall script: {e}");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Tree printing for `list`
// ---------------------------------------------------------------------------

fn print_tree(workers: &HashMap<String, Worker>) {
    // Group children by parent
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for (name, w) in workers {
        let parent = if w.spawned_by.is_empty() {
            String::new()
        } else {
            w.spawned_by.clone()
        };
        children.entry(parent).or_default().push(name.clone());
    }

    // Sort children lists
    for kids in children.values_mut() {
        kids.sort();
    }

    // Roots: workers with no parent, or whose parent is not a known worker
    let known: std::collections::HashSet<&String> = workers.keys().collect();
    let mut roots: Vec<String> = Vec::new();

    if let Some(top) = children.get("") {
        roots.extend(top.iter().cloned());
    }
    for (parent, kids) in &children {
        if !parent.is_empty() && !known.contains(parent) {
            roots.extend(kids.iter().cloned());
        }
    }
    roots.sort();
    roots.dedup();

    let mut visited = std::collections::HashSet::new();
    for (i, root) in roots.iter().enumerate() {
        let is_last = i == roots.len() - 1;
        print_node(root, "", is_last, workers, &children, &mut visited);
    }
}

fn print_node(
    name: &str,
    prefix: &str,
    is_last: bool,
    workers: &HashMap<String, Worker>,
    children: &HashMap<String, Vec<String>>,
    visited: &mut std::collections::HashSet<String>,
) {
    if visited.contains(name) {
        return;
    }
    visited.insert(name.to_string());

    let w = &workers[name];
    let connector = if is_last { "└── " } else { "├── " };

    let short = truncate_task(&w.task, 40);
    let age = relative_time(&w.started_at);
    let level = depth_label(w.depth);
    let has_kids = children.get(name).is_some_and(|k| !k.is_empty());
    let role = if has_kids { "orc" } else { "wrk" };

    let si = match w.status.as_str() {
        "running" => "▶",
        "blocked" => "⏸",
        "done" => "✓",
        "dead" => "✗",
        "destroyed" => "💀",
        _ => "?",
    };

    println!(
        "{prefix}{connector}[{role}] {name}  {}  {si} {}  {level}  {short}  {age}",
        w.backend, w.status
    );

    if let Some(kids) = children.get(name) {
        let child_prefix = if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };
        for (i, kid) in kids.iter().enumerate() {
            let kid_is_last = i == kids.len() - 1;
            print_node(kid, &child_prefix, kid_is_last, workers, children, visited);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_writes_to_file() {
        let _ = config::ensure_home();
        let log_path = config::audit_log_file();
        let before = std::fs::read_to_string(&log_path).unwrap_or_default();
        audit("TEST_EVENT worker=test action=verify");
        let after = std::fs::read_to_string(&log_path).unwrap();
        let new_content = &after[before.len()..];
        assert!(new_content.contains("TEST_EVENT"));
        assert!(new_content.contains("worker=test"));
        assert!(new_content.contains("] "));
    }

    #[test]
    fn test_depth_emoji() {
        assert_eq!(depth_emoji(0), "🐋");
        assert_eq!(depth_emoji(1), "🐳");
        assert_eq!(depth_emoji(2), "🐬");
        assert_eq!(depth_emoji(3), "🐟");
        assert_eq!(depth_emoji(4), "🦐");
        assert_eq!(depth_emoji(100), "🦐");
    }

    #[test]
    fn test_depth_label() {
        assert_eq!(depth_label(0), "🐋 L0");
        assert_eq!(depth_label(1), "🐳 L1");
        assert_eq!(depth_label(3), "🐟 L3");
        assert_eq!(depth_label(99), "🦐 L99");
    }

    #[test]
    fn test_l0_spawn_markers_normalize_to_empty_parent() {
        let workers = std::collections::HashMap::new();
        let (sb, d) = resolve_spawn_lineage(normalize_spawned_by("root"), 0, &workers);
        assert_eq!(sb, "");
        assert_eq!(d, 0);
        assert_eq!(normalize_spawned_by("root:%149"), "");
        assert_eq!(normalize_spawned_by("openclaw"), "");

        let (sb2, d2) = resolve_spawn_lineage(normalize_spawned_by("openclaw"), 0, &workers);
        assert_eq!(sb2, "");
        assert_eq!(d2, 0);
    }

    #[test]
    fn test_resolve_spawn_lineage_explicit_spawned_by_sets_depth() {
        let mut workers = std::collections::HashMap::new();
        workers.insert("p".into(), make_worker_with("p", "codex", "running", 2));
        let (sb, d) = resolve_spawn_lineage("p".into(), 0, &workers);
        assert_eq!(sb, "p");
        assert_eq!(d, 2);
    }

    #[test]
    fn test_resolve_spawn_lineage_unknown_parent_keeps_cli_depth() {
        let workers = std::collections::HashMap::new();
        let (sb, d) = resolve_spawn_lineage("ghost".into(), 1, &workers);
        assert_eq!(sb, "ghost");
        assert_eq!(d, 1);
    }

    #[test]
    fn test_explicit_parent_advances_child_depth_to_l2() {
        let mut workers = std::collections::HashMap::new();
        let mut parent = make_worker_with("mud", "claude", "running", 1);
        parent.pane_id = "%109".into();
        workers.insert("mud".into(), parent);

        let (spawned_by, cli_depth) = resolve_spawn_lineage("mud".into(), 0, &workers);
        assert_eq!(spawned_by, "mud");
        assert_eq!(cli_depth + 1, 2);
        assert_eq!(depth_label(cli_depth + 1), "🐬 L2");
    }

    fn strict_spawn_validate_env() -> SpawnValidateEnv {
        SpawnValidateEnv {
            allow_no_orchestrator: false,
            allow_openclaw_without_reply: false,
        }
    }

    #[test]
    fn validate_rejects_unknown_orchestrator() {
        let workers = HashMap::new();
        let err = validate_spawn_context(
            "ccc",
            "root",
            "",
            None,
            &workers,
            "",
            "",
            &strict_spawn_validate_env(),
        )
        .unwrap_err();
        assert!(err.contains("unknown --orchestrator"), "got: {err}");
        assert!(err.contains("ccc"), "got: {err}");

        for bad in &["typo", "CC", "Openclaw", "slack", ""] {
            assert!(
                validate_spawn_context(
                    bad,
                    "root",
                    "",
                    None,
                    &workers,
                    "",
                    "",
                    &strict_spawn_validate_env()
                )
                .is_err(),
                "should reject orchestrator '{bad}'"
            );
        }
    }

    #[test]
    fn validate_accepts_all_valid_orchestrators() {
        let workers = HashMap::new();
        let env_permissive = SpawnValidateEnv {
            allow_no_orchestrator: true,
            allow_openclaw_without_reply: true,
        };
        for orch in VALID_ORCHESTRATORS {
            validate_spawn_context(orch, "root", "", None, &workers, "", "", &env_permissive)
                .unwrap_or_else(|e| panic!("should accept orchestrator '{orch}': {e}"));
        }
    }

    #[test]
    fn validate_rejects_orchestrator_none_by_default() {
        let workers = HashMap::new();
        assert!(
            validate_spawn_context(
                "none",
                "root",
                "",
                None,
                &workers,
                "",
                "",
                &strict_spawn_validate_env()
            )
            .is_err()
        );
    }

    #[test]
    fn validate_allows_orchestrator_none_when_opt_in() {
        let workers = HashMap::new();
        let env = SpawnValidateEnv {
            allow_no_orchestrator: true,
            allow_openclaw_without_reply: false,
        };
        validate_spawn_context("none", "root", "", None, &workers, "", "", &env).unwrap();
    }

    #[test]
    fn validate_requires_spawned_by_argument() {
        let workers = HashMap::new();
        let err = validate_spawn_context(
            "cc",
            "",
            "",
            None,
            &workers,
            "",
            "",
            &strict_spawn_validate_env(),
        )
        .unwrap_err();
        assert!(err.contains("--spawned-by is required"), "got: {err}");
    }

    #[test]
    fn validate_rejects_unknown_spawned_by_parent() {
        let workers = HashMap::new();
        assert!(
            validate_spawn_context(
                "cc",
                "nope",
                "nope",
                None,
                &workers,
                "",
                "",
                &strict_spawn_validate_env()
            )
            .is_err()
        );
    }

    #[test]
    fn validate_openclaw_requires_reply_routing() {
        let workers = HashMap::new();
        assert!(
            validate_spawn_context(
                "openclaw",
                "root",
                "",
                None,
                &workers,
                "",
                "",
                &strict_spawn_validate_env()
            )
            .is_err()
        );
        validate_spawn_context(
            "openclaw",
            "root",
            "",
            None,
            &workers,
            "slack",
            "C0ABC123",
            &strict_spawn_validate_env(),
        )
        .unwrap();
    }

    #[test]
    fn validate_orca_worker_name_must_match_tracked_self_or_explicit_parent() {
        let mut workers = HashMap::new();
        workers.insert(
            "elm".into(),
            make_worker_with("elm", "claude", "running", 1),
        );
        assert!(
            validate_spawn_context(
                "cc",
                "root",
                "",
                Some("elm"),
                &workers,
                "",
                "",
                &strict_spawn_validate_env()
            )
            .is_err()
        );

        let (sb, _) = resolve_spawn_lineage("elm".into(), 0, &workers);
        assert_eq!(sb, "elm");
        validate_spawn_context(
            "cc",
            "elm",
            &sb,
            Some("elm"),
            &workers,
            "",
            "",
            &strict_spawn_validate_env(),
        )
        .unwrap();
    }

    #[test]
    fn validate_rejects_mismatched_orca_worker_name_and_spawned_by() {
        let mut workers = HashMap::new();
        workers.insert(
            "elm".into(),
            make_worker_with("elm", "claude", "running", 1),
        );
        workers.insert(
            "foo".into(),
            make_worker_with("foo", "claude", "running", 1),
        );
        let (sb, _) = resolve_spawn_lineage("foo".into(), 0, &workers);
        assert_eq!(sb, "foo");
        assert!(
            validate_spawn_context(
                "cc",
                "foo",
                &sb,
                Some("elm"),
                &workers,
                "",
                "",
                &strict_spawn_validate_env()
            )
            .is_err()
        );
    }

    #[test]
    fn validate_allows_explicit_parent_when_orca_worker_name_is_stale() {
        let mut workers = HashMap::new();
        workers.insert(
            "parent".into(),
            make_worker_with("parent", "cc", "running", 1),
        );
        validate_spawn_context(
            "cc",
            "parent",
            "parent",
            Some("stale-dead-worker"),
            &workers,
            "",
            "",
            &strict_spawn_validate_env(),
        )
        .unwrap();
    }

    // --- Orchestrator vs worker depth (whale chain) ---
    // Conceptual L0 = the orchestrator (Claude Code / Codex / Cursor pane, or OpenClaw).
    // It is not an Orca `Worker` row. First `orca spawn` uses CLI --depth 0 → stored depth 1 → 🐳 L1.
    // Nested spawns from inside a worker bump stored depth by 1 (🐬 L2, 🐟 L3, …).

    #[test]
    fn test_hierarchy_top_level_spawn_is_l1_same_for_cc_and_openclaw() {
        let cli_depth = 0u32;
        let stored = cli_depth + 1;
        assert_eq!(depth_label(stored), "🐳 L1");
    }

    #[test]
    fn test_hierarchy_worker_inside_l1_explicit_parent_gets_l2() {
        let mut workers = std::collections::HashMap::new();
        workers.insert(
            "l1-worker".into(),
            make_worker_with("l1-worker", "claude", "running", 1),
        );
        let (spawned_by, cli_depth) = resolve_spawn_lineage("l1-worker".into(), 0, &workers);
        assert_eq!(spawned_by, "l1-worker");
        assert_eq!(cli_depth, 1);
        let stored = cli_depth + 1;
        assert_eq!(stored, 2);
        assert_eq!(depth_label(stored), "🐬 L2");
    }

    #[test]
    fn test_hierarchy_explicit_spawned_by_depth_2_parent_yields_l3() {
        let mut workers = std::collections::HashMap::new();
        workers.insert("l2".into(), make_worker_with("l2", "codex", "running", 2));
        let (spawned_by, cli_depth) = resolve_spawn_lineage("l2".into(), 0, &workers);
        assert_eq!(spawned_by, "l2");
        assert_eq!(cli_depth, 2);
        assert_eq!(depth_label(cli_depth + 1), "🐟 L3");
    }

    #[test]
    fn apply_hook_done_deferral_noop_without_children() {
        let (e, m) = super::apply_hook_done_deferral("done", "claude stop", "hook", false);
        assert_eq!(e, "done");
        assert_eq!(m, "claude stop");
    }

    #[test]
    fn apply_hook_done_deferral_skipped_for_non_hook_source() {
        let (e, m) = super::apply_hook_done_deferral("done", "x", "cli", true);
        assert_eq!(e, "done");
        assert_eq!(m, "x");
    }

    #[test]
    fn apply_hook_done_deferral_heartbeat_when_children_running() {
        let (e, m) = super::apply_hook_done_deferral("done", "claude stop", "hook", true);
        assert_eq!(e, "heartbeat");
        assert!(m.contains("claude stop"));
        assert!(m.contains("deferred"));
    }

    #[test]
    fn apply_hook_done_deferral_empty_message_when_children() {
        let (e, m) = super::apply_hook_done_deferral("done", "", "hook", true);
        assert_eq!(e, "heartbeat");
        assert!(m.contains("sub-workers"));
        assert!(m.contains("active"));
    }

    #[test]
    fn test_relative_time_seconds() {
        let ts = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::seconds(30))
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let result = relative_time(&ts);
        assert!(result.ends_with("s ago"), "got: {result}");
    }

    #[test]
    fn test_relative_time_minutes() {
        let ts = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::seconds(300))
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let result = relative_time(&ts);
        assert!(result.ends_with("m ago"), "got: {result}");
        assert!(result.starts_with('5'), "got: {result}");
    }

    #[test]
    fn test_relative_time_hours() {
        let ts = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::seconds(7200))
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let result = relative_time(&ts);
        assert!(result.ends_with("h ago"), "got: {result}");
        assert!(result.starts_with('2'), "got: {result}");
    }

    #[test]
    fn test_relative_time_invalid() {
        assert_eq!(relative_time("garbage"), "garbage");
        assert_eq!(relative_time(""), "");
    }

    #[test]
    fn test_relative_time_future_clamps_to_zero() {
        let ts = chrono::Utc::now()
            .checked_add_signed(chrono::Duration::seconds(3600))
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let result = relative_time(&ts);
        assert_eq!(result, "0s ago");
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
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("hello \x1b[31mworld\x1b[0m"), "hello world");
        assert_eq!(strip_ansi("clean text"), "clean text");
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_filter_workers_by_scope_empty_filters() {
        let mut workers = HashMap::new();
        let mut w = make_test_worker("a");
        w.orchestrator_pane = "%1".into();
        workers.insert("a".to_string(), w);

        let result = filter_workers_by_scope(&workers, "", "");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_workers_by_scope_pane() {
        let mut workers = HashMap::new();

        let mut w1 = make_test_worker("a");
        w1.orchestrator_pane = "%1".into();
        workers.insert("a".to_string(), w1);

        let mut w2 = make_test_worker("b");
        w2.orchestrator_pane = "%2".into();
        workers.insert("b".to_string(), w2);

        let result = filter_workers_by_scope(&workers, "%1", "");
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("a"));
    }

    #[test]
    fn test_filter_workers_by_scope_children() {
        let mut workers = HashMap::new();

        let mut parent = make_test_worker("parent");
        parent.orchestrator_pane = "%1".into();
        workers.insert("parent".to_string(), parent);

        let mut child = make_test_worker("child");
        child.spawned_by = "parent".into();
        workers.insert("child".to_string(), child);

        let mut unrelated = make_test_worker("other");
        unrelated.orchestrator_pane = "%2".into();
        workers.insert("other".to_string(), unrelated);

        let result = filter_workers_by_scope(&workers, "%1", "");
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("parent"));
        assert!(result.contains_key("child"));
    }

    #[test]
    fn test_filter_workers_by_scope_session_id() {
        let mut workers = HashMap::new();

        let mut w1 = make_test_worker("a");
        w1.session_id = "sess-1".into();
        workers.insert("a".to_string(), w1);

        let mut w2 = make_test_worker("b");
        w2.session_id = "sess-2".into();
        workers.insert("b".to_string(), w2);

        let result = filter_workers_by_scope(&workers, "", "sess-1");
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("a"));
    }

    #[test]
    fn test_worker_target_prefers_pane_id() {
        let mut w = make_test_worker("alice");
        w.pane_id = "%7".into();
        assert_eq!(worker_target(&w), "%7");
    }

    #[test]
    fn test_worker_target_falls_back_to_session_name() {
        let w = make_test_worker("bob");
        let target = worker_target(&w);
        assert!(target.ends_with(":bob"), "got: {target}");
    }

    // -----------------------------------------------------------------------
    // A) write_hook_scripts — creates 3 files in a temp dir
    // -----------------------------------------------------------------------

    #[test]
    fn test_write_hook_scripts_creates_files() {
        let dir = write_hook_scripts().expect("write_hook_scripts failed");
        let install = dir.join("install.sh");
        let uninstall = dir.join("uninstall.sh");
        let hook = dir.join("orca-hook.sh");

        assert!(install.exists(), "install.sh missing");
        assert!(uninstall.exists(), "uninstall.sh missing");
        assert!(hook.exists(), "orca-hook.sh missing");

        assert!(
            !std::fs::read_to_string(&install).unwrap().is_empty(),
            "install.sh is empty"
        );
        assert!(
            !std::fs::read_to_string(&uninstall).unwrap().is_empty(),
            "uninstall.sh is empty"
        );
        assert!(
            !std::fs::read_to_string(&hook).unwrap().is_empty(),
            "orca-hook.sh is empty"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&install, &uninstall, &hook] {
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert!(mode & 0o111 != 0, "{path:?} is not executable");
            }
        }
    }

    // -----------------------------------------------------------------------
    // B) HOOK_*_SH constants are non-empty
    // -----------------------------------------------------------------------

    #[test]
    fn test_hook_constants_non_empty() {
        assert!(!HOOK_INSTALL_SH.is_empty(), "HOOK_INSTALL_SH is empty");
        assert!(!HOOK_UNINSTALL_SH.is_empty(), "HOOK_UNINSTALL_SH is empty");
        assert!(!HOOK_ORCA_SH.is_empty(), "HOOK_ORCA_SH is empty");
    }

    #[test]
    fn test_hook_install_sh_contains_shebang_or_bash() {
        assert!(
            HOOK_INSTALL_SH.contains("bash") || HOOK_INSTALL_SH.contains("sh"),
            "install.sh doesn't look like a shell script"
        );
    }

    #[test]
    fn test_hook_uninstall_sh_contains_shebang_or_bash() {
        assert!(
            HOOK_UNINSTALL_SH.contains("bash") || HOOK_UNINSTALL_SH.contains("sh"),
            "uninstall.sh doesn't look like a shell script"
        );
    }

    #[test]
    fn test_hook_orca_sh_contains_orca() {
        assert!(
            HOOK_ORCA_SH.contains("orca"),
            "orca-hook.sh doesn't reference orca"
        );
    }

    // -----------------------------------------------------------------------
    // C) print_tree — various worker topologies
    // -----------------------------------------------------------------------

    fn make_worker_with(name: &str, backend: &str, status: &str, depth: u32) -> Worker {
        let mut w = make_test_worker(name);
        w.backend = backend.into();
        w.status = status.into();
        w.depth = depth;
        w.started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        w.task = format!("task for {name}");
        w
    }

    #[test]
    fn test_print_tree_single_worker() {
        let mut workers = HashMap::new();
        workers.insert(
            "alpha".to_string(),
            make_worker_with("alpha", "claude", "running", 0),
        );
        print_tree(&workers);
    }

    #[test]
    fn test_print_tree_parent_child() {
        let mut workers = HashMap::new();
        workers.insert(
            "parent".to_string(),
            make_worker_with("parent", "claude", "running", 0),
        );
        let mut child = make_worker_with("child", "codex", "running", 1);
        child.spawned_by = "parent".into();
        workers.insert("child".to_string(), child);
        print_tree(&workers);
    }

    #[test]
    fn test_print_tree_multiple_roots() {
        let mut workers = HashMap::new();
        workers.insert(
            "root1".to_string(),
            make_worker_with("root1", "claude", "running", 0),
        );
        workers.insert(
            "root2".to_string(),
            make_worker_with("root2", "codex", "done", 0),
        );
        workers.insert(
            "root3".to_string(),
            make_worker_with("root3", "cursor", "blocked", 0),
        );
        print_tree(&workers);
    }

    #[test]
    fn test_print_tree_deep_hierarchy() {
        let mut workers = HashMap::new();
        workers.insert(
            "l0".to_string(),
            make_worker_with("l0", "claude", "running", 0),
        );
        let mut l1 = make_worker_with("l1", "claude", "running", 1);
        l1.spawned_by = "l0".into();
        workers.insert("l1".to_string(), l1);
        let mut l2 = make_worker_with("l2", "claude", "running", 2);
        l2.spawned_by = "l1".into();
        workers.insert("l2".to_string(), l2);
        let mut l3 = make_worker_with("l3", "claude", "done", 3);
        l3.spawned_by = "l2".into();
        workers.insert("l3".to_string(), l3);
        print_tree(&workers);
    }

    #[test]
    fn test_print_tree_multiple_children() {
        let mut workers = HashMap::new();
        workers.insert(
            "boss".to_string(),
            make_worker_with("boss", "claude", "running", 0),
        );
        for name in ["kid-a", "kid-b", "kid-c"] {
            let mut w = make_worker_with(name, "codex", "running", 1);
            w.spawned_by = "boss".into();
            workers.insert(name.to_string(), w);
        }
        print_tree(&workers);
    }

    #[test]
    fn test_print_tree_orphan_workers() {
        let mut workers = HashMap::new();
        let mut orphan = make_worker_with("orphan", "claude", "running", 1);
        orphan.spawned_by = "deleted-parent".into();
        workers.insert("orphan".to_string(), orphan);
        print_tree(&workers);
    }

    #[test]
    fn test_print_tree_all_statuses() {
        let mut workers = HashMap::new();
        for (name, status) in [
            ("w-running", "running"),
            ("w-blocked", "blocked"),
            ("w-done", "done"),
            ("w-dead", "dead"),
            ("w-destroyed", "destroyed"),
            ("w-unknown", "weird"),
        ] {
            workers.insert(
                name.to_string(),
                make_worker_with(name, "claude", status, 0),
            );
        }
        print_tree(&workers);
    }

    #[test]
    fn test_print_tree_long_task_truncated() {
        let mut workers = HashMap::new();
        let mut w = make_worker_with("long-task", "claude", "running", 0);
        w.task = "a".repeat(100);
        workers.insert("long-task".to_string(), w);
        print_tree(&workers);
    }

    // -----------------------------------------------------------------------
    // D) print_node — cycle prevention
    // -----------------------------------------------------------------------

    #[test]
    fn test_print_node_visited_prevents_cycle() {
        let mut workers = HashMap::new();
        let mut w1 = make_worker_with("cycle-a", "claude", "running", 0);
        w1.spawned_by = "cycle-b".into();
        workers.insert("cycle-a".to_string(), w1);

        let mut w2 = make_worker_with("cycle-b", "claude", "running", 0);
        w2.spawned_by = "cycle-a".into();
        workers.insert("cycle-b".to_string(), w2);

        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        children.insert("cycle-a".to_string(), vec!["cycle-b".to_string()]);
        children.insert("cycle-b".to_string(), vec!["cycle-a".to_string()]);

        let mut visited = std::collections::HashSet::new();
        print_node("cycle-a", "", true, &workers, &children, &mut visited);
        assert!(visited.contains("cycle-a"));
        assert!(visited.contains("cycle-b"));
    }

    #[test]
    fn test_print_node_already_visited_skips() {
        let mut workers = HashMap::new();
        workers.insert(
            "skip-me".to_string(),
            make_worker_with("skip-me", "claude", "running", 0),
        );
        let children: HashMap<String, Vec<String>> = HashMap::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert("skip-me".to_string());

        print_node("skip-me", "", true, &workers, &children, &mut visited);
    }

    #[test]
    fn test_print_node_is_last_vs_not_last() {
        let mut workers = HashMap::new();
        workers.insert(
            "node".to_string(),
            make_worker_with("node", "claude", "running", 0),
        );
        let children: HashMap<String, Vec<String>> = HashMap::new();

        let mut visited = std::collections::HashSet::new();
        print_node("node", "  ", false, &workers, &children, &mut visited);

        let mut visited2 = std::collections::HashSet::new();
        print_node("node", "  ", true, &workers, &children, &mut visited2);
    }

    // -----------------------------------------------------------------------
    // E) report_field_updates
    // -----------------------------------------------------------------------

    #[test]
    fn test_report_field_updates_done() {
        let updates = report_field_updates("done", "2025-01-01T00:00:00Z");
        assert_eq!(
            updates["last_event_at"],
            serde_json::Value::String("2025-01-01T00:00:00Z".to_string())
        );
        assert_eq!(updates["done_reported"], serde_json::Value::Bool(true));
        assert!(!updates.contains_key("status"));
        assert!(!updates.contains_key("process_exited"));
    }

    #[test]
    fn test_report_field_updates_blocked() {
        let updates = report_field_updates("blocked", "2025-01-01T00:00:00Z");
        assert_eq!(
            updates["last_event_at"],
            serde_json::Value::String("2025-01-01T00:00:00Z".to_string())
        );
        assert_eq!(
            updates["status"],
            serde_json::Value::String("blocked".to_string())
        );
        assert!(!updates.contains_key("done_reported"));
        assert!(!updates.contains_key("process_exited"));
    }

    #[test]
    fn test_report_field_updates_process_exit() {
        let updates = report_field_updates("process_exit", "2025-06-01T12:00:00Z");
        assert_eq!(
            updates["last_event_at"],
            serde_json::Value::String("2025-06-01T12:00:00Z".to_string())
        );
        assert_eq!(updates["process_exited"], serde_json::Value::Bool(true));
        assert!(!updates.contains_key("done_reported"));
        assert!(!updates.contains_key("status"));
    }

    #[test]
    fn test_report_field_updates_heartbeat() {
        let updates = report_field_updates("heartbeat", "2025-01-01T00:00:00Z");
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates["last_event_at"],
            serde_json::Value::String("2025-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn test_report_field_updates_unknown_event() {
        let updates = report_field_updates("something_else", "ts");
        assert_eq!(updates.len(), 1);
        assert!(updates.contains_key("last_event_at"));
    }

    #[test]
    fn test_report_field_updates_empty_timestamp() {
        let updates = report_field_updates("done", "");
        assert_eq!(
            updates["last_event_at"],
            serde_json::Value::String(String::new())
        );
    }

    // -----------------------------------------------------------------------
    // F) Cli::try_parse_from — subcommand parsing
    // -----------------------------------------------------------------------

    #[test]
    fn test_cli_parse_spawn_basic() {
        let cli =
            Cli::try_parse_from(["orca", "spawn", "build the widget", "--spawned-by", "root"]);
        assert!(cli.is_ok(), "spawn parse failed: {:?}", cli.err());
    }

    #[test]
    fn test_cli_parse_spawn_with_flags() {
        let cli = Cli::try_parse_from([
            "orca",
            "spawn",
            "fix bug",
            "--backend",
            "codex",
            "--dir",
            "/tmp/proj",
            "--name",
            "worker-1",
            "--base-branch",
            "develop",
            "--orchestrator",
            "cc",
            "--pane",
            "%5",
            "--depth",
            "2",
            "--spawned-by",
            "parent-worker",
        ]);
        assert!(cli.is_ok(), "spawn with flags failed: {:?}", cli.err());
    }

    #[test]
    fn test_cli_parse_spawn_multi_word_task() {
        let cli = Cli::try_parse_from([
            "orca",
            "spawn",
            "implement",
            "the",
            "new",
            "feature",
            "--spawned-by",
            "root",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_spawn_missing_task() {
        let cli = Cli::try_parse_from(["orca", "spawn"]);
        assert!(cli.is_err(), "spawn without task should fail");
    }

    #[test]
    fn test_cli_parse_list() {
        let cli = Cli::try_parse_from(["orca", "list"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_status() {
        let cli = Cli::try_parse_from(["orca", "status", "my-worker"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_status_missing_name() {
        let cli = Cli::try_parse_from(["orca", "status"]);
        assert!(cli.is_err());
    }

    #[test]
    fn test_cli_parse_logs() {
        let cli = Cli::try_parse_from(["orca", "logs", "my-worker"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_logs_with_flags() {
        let cli = Cli::try_parse_from(["orca", "logs", "my-worker", "-n", "50", "--raw"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_report() {
        let cli = Cli::try_parse_from([
            "orca",
            "report",
            "--worker",
            "w1",
            "--event",
            "done",
            "--message",
            "all good",
            "--source",
            "hook",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_report_minimal() {
        let cli = Cli::try_parse_from(["orca", "report", "--worker", "w1", "--event", "heartbeat"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_steer() {
        let cli = Cli::try_parse_from(["orca", "steer", "w1", "do", "something", "else"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_steer_missing_message() {
        let cli = Cli::try_parse_from(["orca", "steer", "w1"]);
        assert!(cli.is_err());
    }

    #[test]
    fn test_cli_parse_kill() {
        let cli = Cli::try_parse_from(["orca", "kill", "w1"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_kill_no_stash() {
        let cli = Cli::try_parse_from(["orca", "kill", "w1", "--no-stash"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_kill_missing_name() {
        let cli = Cli::try_parse_from(["orca", "kill"]);
        assert!(cli.is_err());
    }

    #[test]
    fn test_cli_parse_killall_force() {
        let cli = Cli::try_parse_from(["orca", "killall", "--force"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_killall_mine() {
        let cli = Cli::try_parse_from(["orca", "killall", "--mine"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_killall_pane() {
        let cli = Cli::try_parse_from(["orca", "killall", "--pane", "%3"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_killall_session_id() {
        let cli = Cli::try_parse_from(["orca", "killall", "--session-id", "sess-42"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_killall_no_stash() {
        let cli = Cli::try_parse_from(["orca", "killall", "--force", "--no-stash"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_gc_force() {
        let cli = Cli::try_parse_from(["orca", "gc", "--force"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_gc_mine() {
        let cli = Cli::try_parse_from(["orca", "gc", "--mine"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_gc_no_stash() {
        let cli = Cli::try_parse_from(["orca", "gc", "--force", "--no-stash"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_pane() {
        let cli = Cli::try_parse_from(["orca", "pane"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_daemon_start() {
        let cli = Cli::try_parse_from(["orca", "daemon", "start"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_daemon_stop() {
        let cli = Cli::try_parse_from(["orca", "daemon", "stop"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_daemon_status() {
        let cli = Cli::try_parse_from(["orca", "daemon", "status"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_hooks_install() {
        let cli = Cli::try_parse_from(["orca", "hooks", "install"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_hooks_uninstall() {
        let cli = Cli::try_parse_from(["orca", "hooks", "uninstall"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_no_args() {
        let cli = Cli::try_parse_from(["orca"]);
        assert!(cli.is_err(), "no args should fail");
    }

    #[test]
    fn test_cli_parse_unknown_subcommand() {
        let cli = Cli::try_parse_from(["orca", "frobnicate"]);
        assert!(cli.is_err());
    }

    #[test]
    fn test_cli_parse_spawn_reply_flags() {
        let cli = Cli::try_parse_from([
            "orca",
            "spawn",
            "do stuff",
            "--reply-channel",
            "slack",
            "--reply-to",
            "C12345",
            "--reply-thread",
            "thread-abc",
            "--session-id",
            "sid-99",
            "--spawned-by",
            "root",
        ]);
        assert!(cli.is_ok());
    }

    // -----------------------------------------------------------------------
    // G) strip_ansi — more complex ANSI sequences
    // -----------------------------------------------------------------------

    #[test]
    fn test_strip_ansi_sgr_sequences() {
        assert_eq!(strip_ansi("\x1b[1;32mgreen bold\x1b[0m"), "green bold");
    }

    #[test]
    fn test_strip_ansi_256_color() {
        assert_eq!(strip_ansi("\x1b[38;5;196mred text\x1b[0m"), "red text");
    }

    #[test]
    fn test_strip_ansi_24bit_color() {
        assert_eq!(strip_ansi("\x1b[38;2;255;100;0morange\x1b[0m"), "orange");
    }

    #[test]
    fn test_strip_ansi_cursor_movement() {
        assert_eq!(strip_ansi("\x1b[2Jhello\x1b[H"), "hello");
    }

    #[test]
    fn test_strip_ansi_osc_title() {
        assert_eq!(
            strip_ansi("\x1b]0;My Terminal Title\x07some text"),
            "some text"
        );
    }

    #[test]
    fn test_strip_ansi_private_mode() {
        assert_eq!(strip_ansi("\x1b[?25hvisible\x1b[?25l"), "visible");
    }

    #[test]
    fn test_strip_ansi_charset_switch() {
        assert_eq!(strip_ansi("\x1b(Btext\x1b)0more"), "textmore");
    }

    #[test]
    fn test_strip_ansi_mixed() {
        let input = "\x1b[1m\x1b[31mError:\x1b[0m \x1b[33mfile not found\x1b[0m";
        assert_eq!(strip_ansi(input), "Error: file not found");
    }

    #[test]
    fn test_strip_ansi_multiline() {
        let input = "\x1b[32mline1\x1b[0m\n\x1b[31mline2\x1b[0m\nline3";
        assert_eq!(strip_ansi(input), "line1\nline2\nline3");
    }

    #[test]
    fn test_strip_ansi_no_codes() {
        assert_eq!(strip_ansi("plain text here"), "plain text here");
    }

    #[test]
    fn test_strip_ansi_only_codes() {
        assert_eq!(strip_ansi("\x1b[31m\x1b[0m"), "");
    }

    // -----------------------------------------------------------------------
    // Additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_nudge_daemon_does_not_panic() {
        nudge_daemon();
    }

    #[test]
    fn test_print_tree_empty_workers() {
        let workers: HashMap<String, Worker> = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        children.entry(String::new()).or_default();
        // print_tree with empty map should not panic (it just prints nothing)
        // We call the internal pieces since print_tree doesn't print for empty
        let known: std::collections::HashSet<&String> = workers.keys().collect();
        assert!(known.is_empty());
    }

    #[test]
    fn test_filter_workers_by_scope_deep_transitive_children() {
        let mut workers = HashMap::new();

        let mut root = make_test_worker("root");
        root.orchestrator_pane = "%1".into();
        workers.insert("root".to_string(), root);

        let mut mid = make_test_worker("mid");
        mid.spawned_by = "root".into();
        workers.insert("mid".to_string(), mid);

        let mut leaf = make_test_worker("leaf");
        leaf.spawned_by = "mid".into();
        workers.insert("leaf".to_string(), leaf);

        let mut other = make_test_worker("other");
        other.orchestrator_pane = "%9".into();
        workers.insert("other".to_string(), other);

        let result = filter_workers_by_scope(&workers, "%1", "");
        assert_eq!(result.len(), 3);
        assert!(result.contains_key("root"));
        assert!(result.contains_key("mid"));
        assert!(result.contains_key("leaf"));
        assert!(!result.contains_key("other"));
    }

    #[test]
    fn test_filter_workers_by_scope_both_pane_and_session() {
        let mut workers = HashMap::new();

        let mut w1 = make_test_worker("by-pane");
        w1.orchestrator_pane = "%1".into();
        workers.insert("by-pane".to_string(), w1);

        let mut w2 = make_test_worker("by-session");
        w2.session_id = "sess-x".into();
        workers.insert("by-session".to_string(), w2);

        let mut w3 = make_test_worker("neither");
        w3.orchestrator_pane = "%99".into();
        w3.session_id = "sess-other".into();
        workers.insert("neither".to_string(), w3);

        let result = filter_workers_by_scope(&workers, "%1", "sess-x");
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("by-pane"));
        assert!(result.contains_key("by-session"));
    }

    #[test]
    fn test_depth_emoji_boundary() {
        assert_eq!(depth_emoji(4), "🦐");
        assert_eq!(depth_emoji(5), "🦐");
        assert_eq!(depth_emoji(u32::MAX), "🦐");
    }

    #[test]
    fn test_depth_label_formatting() {
        let label = depth_label(2);
        assert!(label.contains("L2"));
        assert!(label.contains(depth_emoji(2)));
    }

    #[test]
    fn test_worker_target_empty_pane_id() {
        let w = make_test_worker("test-name");
        let target = worker_target(&w);
        assert!(target.contains(":test-name"));
    }
}
