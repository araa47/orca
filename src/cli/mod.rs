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

const L0_SPAWN_MARKERS: &[&str] = &["root", "openclaw", "self"];

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

/// When `--spawned-by` names a known parent worker (including auto-registered
/// L0 entries), derive the child depth as `parent.depth + 1`.
fn resolve_spawn_lineage(
    spawned_by: String,
    mut depth: u32,
    workers: &std::collections::HashMap<String, Worker>,
) -> (String, u32) {
    if !spawned_by.is_empty() {
        if let Some(parent) = workers.get(&spawned_by) {
            depth = parent.depth + 1;
        } else if is_root_spawn_marker(&spawned_by) {
            // L0 marker not yet in state (edge case) → child is L1
            depth = 1;
        }
    }
    (spawned_by, depth)
}

// ---------------------------------------------------------------------------
// L0 orchestrator auto-registration
// ---------------------------------------------------------------------------

fn make_l0_worker(
    name: &str,
    backend: &str,
    pane_id: &str,
    dir: &str,
    session_id: &str,
    base_branch: &str,
) -> Worker {
    Worker {
        name: name.to_string(),
        backend: backend.to_string(),
        task: String::new(),
        dir: dir.to_string(),
        workdir: dir.to_string(),
        base_branch: base_branch.to_string(),
        orchestrator: backend.to_string(),
        orchestrator_pane: String::new(),
        session_id: session_id.to_string(),
        reply_channel: String::new(),
        reply_to: String::new(),
        reply_thread: String::new(),
        pane_id: pane_id.to_string(),
        depth: 0,
        spawned_by: String::new(),
        layout: "window".to_string(),
        status: "running".to_string(),
        started_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        last_event_at: String::new(),
        done_reported: false,
        process_exited: false,
    }
}

/// Auto-register an L0 orchestrator entry in state when `--spawned-by` is a
/// root marker. Returns the L0 entry's name (e.g. `"openclaw"` or a generated
/// name for cc/cx/cu).
#[allow(clippy::too_many_arguments)]
fn ensure_l0_orchestrator(
    raw_spawned_by: &str,
    orchestrator: &str,
    pane: &str,
    project_dir: &str,
    session_id: &str,
    base_branch: &str,
    workers: &mut HashMap<String, Worker>,
) -> Result<String, String> {
    let trimmed = raw_spawned_by.trim();

    // --spawned-by openclaw is only valid with --orchestrator openclaw
    if trimmed == "openclaw" && orchestrator != "openclaw" {
        return Err(
            "--spawned-by openclaw is only for the OpenClaw orchestrator. \
             For cc/cx/cu L0, use --spawned-by self."
                .into(),
        );
    }

    // Determine if this should be an openclaw L0 entry
    let is_openclaw_l0 = trimmed == "openclaw"
        || (orchestrator == "openclaw" && (trimmed == "root" || trimmed.starts_with("root:")));

    if is_openclaw_l0 {
        if !workers.contains_key("openclaw") {
            let w = make_l0_worker(
                "openclaw",
                "openclaw",
                "",
                project_dir,
                session_id,
                base_branch,
            );
            state::save_worker(&w, false)
                .map_err(|e| format!("Failed to register L0 orchestrator: {e}"))?;
            workers.insert("openclaw".to_string(), w);
        }
        return Ok("openclaw".to_string());
    }

    // cc/cx/cu L0: find existing entry by pane or create a new one.
    // Defense-in-depth: if the pane already belongs to a tracked worker at
    // depth > 0, return that worker's name instead of creating a ghost L0.
    if !pane.is_empty() {
        for (name, w) in workers.iter() {
            if w.pane_id == pane {
                if w.depth > 0 {
                    return Ok(name.clone());
                }
                if w.spawned_by.is_empty() {
                    return Ok(name.clone());
                }
            }
        }
    }

    // Generate new L0 entry
    let existing: HashSet<String> = workers.keys().cloned().collect();
    let l0_name = crate::names::generate_name(&existing)
        .map_err(|e| format!("Failed to generate L0 name: {e}"))?;
    let backend = config::canonical_backend(orchestrator);
    let w = make_l0_worker(
        &l0_name,
        backend,
        pane,
        project_dir,
        session_id,
        base_branch,
    );
    state::save_worker(&w, false)
        .map_err(|e| format!("Failed to register L0 orchestrator: {e}"))?;
    workers.insert(l0_name.clone(), w);

    // Rename tmux window with L0 emoji
    if !pane.is_empty() {
        let l0_display = format!("🐋{l0_name}");
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            tmux::tmux(&["set-option", "-wt", pane, "automatic-rename", "off"]).await;
            tmux::tmux(&["rename-window", "-t", pane, &l0_display]).await;
            let l0_title = format!("🐋 {l0_name} [L0]");
            tmux::tmux(&["select-pane", "-t", pane, "-T", &l0_title]).await;
        });
    }

    Ok(l0_name)
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
    if !spawned_by.is_empty()
        && !is_root_spawn_marker(spawned_by)
        && !workers.contains_key(spawned_by)
    {
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
                    if is_root_spawn_marker(spawned_by) {
                        "openclaw"
                    } else {
                        spawned_by
                    }
                ));
            }
        } else if is_root_spawn_marker(raw_spawned_by) || spawned_by.is_empty() {
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

    let mut workers = state::load_workers();
    let implicit_env = std::env::var("ORCA_WORKER_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    let raw_spawned_by = spawned_by;

    // Pre-validate before any side effects (L0 registration, tmux rename)
    let spawn_env = SpawnValidateEnv::from_process_env();
    if let Err(msg) = validate_spawn_context(
        &orchestrator,
        &raw_spawned_by,
        &raw_spawned_by,
        implicit_env.as_deref(),
        &workers,
        &reply_channel,
        &reply_to,
        &spawn_env,
    ) {
        eprintln!("{msg}");
        process::exit(1);
    }

    // Resolve `--spawned-by self` to the actual worker name when the calling
    // pane already belongs to a tracked worker (depth > 0).  Cursor workers
    // don't have ORCA_WORKER_NAME in their env, so they fall back to `self`
    // even when they are L2+ — without this resolution they would accidentally
    // register a ghost L0 entry via ensure_l0_orchestrator.
    let raw_spawned_by = if raw_spawned_by == "self" && !pane.is_empty() {
        workers
            .iter()
            .find(|(_, w)| w.pane_id == pane && w.depth > 0)
            .map(|(name, _)| name.clone())
            .unwrap_or(raw_spawned_by)
    } else {
        raw_spawned_by
    };

    // If this is an L0 root marker, auto-register the L0 orchestrator entry
    let spawned_by = if is_root_spawn_marker(&raw_spawned_by) {
        match ensure_l0_orchestrator(
            &raw_spawned_by,
            &orchestrator,
            &pane,
            &project_dir,
            &session_id,
            &base_branch,
            &mut workers,
        ) {
            Ok(name) => name,
            Err(msg) => {
                eprintln!("Error: {msg}");
                process::exit(1);
            }
        }
    } else {
        raw_spawned_by.clone()
    };
    let (spawned_by, depth) = resolve_spawn_lineage(spawned_by, depth, &workers);

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

    if !daemon::is_daemon_running() && daemon::can_reach_tmux() {
        let pid = daemon::start_daemon_background();
        if pid > 0 {
            println!("Daemon started (pid={pid})");
        }
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

    // Protect L0 orchestrator entries — they represent the orchestrator itself,
    // not a spawned worker. Killing them would destroy the orchestrator's pane.
    if w.depth == 0 && w.spawned_by.is_empty() {
        eprintln!(
            "Error: '{name}' is an L0 orchestrator entry and cannot be killed. \
             Use `orca killall` to clean up its workers instead."
        );
        process::exit(1);
    }

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

    // Filter out L0 orchestrator entries — they are bookkeeping, not killable workers
    let killable: HashMap<String, Worker> = workers
        .into_iter()
        .filter(|(_, w)| !(w.depth == 0 && w.spawned_by.is_empty()))
        .collect();
    if killable.is_empty() {
        println!("No workers to kill (L0 orchestrator entries are excluded).");
        return;
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    for (wname, w) in &killable {
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

    let killed_names: Vec<String> = killable.keys().cloned().collect();
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
        .filter(|(_, w)| {
            // Skip L0 orchestrator entries — they are bookkeeping, not GC-able
            if w.depth == 0 && w.spawned_by.is_empty() {
                return false;
            }
            matches!(w.status.as_str(), "done" | "dead" | "destroyed")
        })
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

const HOOK_INSTALL_SH: &str = include_str!("../../hooks/install.sh");
const HOOK_UNINSTALL_SH: &str = include_str!("../../hooks/uninstall.sh");
const HOOK_ORCA_SH: &str = include_str!("../../hooks/orca-hook.sh");

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
    // Separate L0 orchestrator entries from real workers
    let mut l0_entries: Vec<&Worker> = Vec::new();
    let mut real_workers: HashMap<&String, &Worker> = HashMap::new();
    for (name, w) in workers {
        if w.depth == 0 && w.spawned_by.is_empty() {
            l0_entries.push(w);
        } else {
            real_workers.insert(name, w);
        }
    }
    l0_entries.sort_by(|a, b| a.name.cmp(&b.name));

    // Group children by parent
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for (name, w) in workers {
        if w.depth == 0 && w.spawned_by.is_empty() {
            continue; // L0 entries are roots, not children
        }
        children
            .entry(w.spawned_by.clone())
            .or_default()
            .push(name.clone());
    }
    for kids in children.values_mut() {
        kids.sort();
    }

    // Collect L0 names that have children
    let known: HashSet<&String> = workers.keys().collect();

    // Build L0 groups: each L0 entry OR orphaned parent gets a header
    let mut l0_groups: Vec<(String, String, Vec<String>)> = Vec::new(); // (name, backend, children)

    // First: registered L0 entries
    for l0 in &l0_entries {
        let kids = children.get(&l0.name).cloned().unwrap_or_default();
        l0_groups.push((l0.name.clone(), l0.backend.clone(), kids));
    }

    // Workers with empty spawned_by that aren't L0 entries themselves
    if let Some(orphans) = children.get("") {
        let l0_names: HashSet<&str> = l0_entries.iter().map(|w| w.name.as_str()).collect();
        let real_orphans: Vec<String> = orphans
            .iter()
            .filter(|n| !l0_names.contains(n.as_str()))
            .cloned()
            .collect();
        if !real_orphans.is_empty() {
            l0_groups.push(("(unknown)".to_string(), String::new(), real_orphans));
        }
    }

    // Orphaned children whose parent is not a known worker
    for (parent, kids) in &children {
        if !parent.is_empty()
            && !known.contains(parent)
            && !l0_groups.iter().any(|(n, _, _)| n == parent)
        {
            l0_groups.push((parent.clone(), String::new(), kids.clone()));
        }
    }

    let mut visited = HashSet::new();
    for (gi, (l0_name, l0_backend, roots)) in l0_groups.iter().enumerate() {
        let l0_label = if l0_backend.is_empty() {
            format!("L0 {l0_name}")
        } else {
            format!("L0 {l0_name} ({l0_backend})")
        };
        println!("{}|-\u{1f40b} | {l0_label}", if gi > 0 { "\n" } else { "" });
        for (i, root) in roots.iter().enumerate() {
            let is_last = i == roots.len() - 1;
            print_node(root, "", is_last, workers, &children, &mut visited);
        }
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
mod tests;
