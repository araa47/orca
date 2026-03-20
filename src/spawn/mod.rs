//! Full spawn flow: git init -> worktree -> tmux window -> launch agent -> save state.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::config;
use crate::config::{canonical_backend, cli_config, ensure_home, orca_home, tmux_session};
use crate::names::generate_name;
use crate::state::{Worker, remove_worker, save_worker, worker_names};
use crate::tmux::{
    capture_pane, create_window, ensure_session, kill_pane, kill_window, pane_alive, send_keys,
    tmux, wait_for_running, window_exists,
};
use crate::worktree::{create_worktree, ensure_git_repo, remove_worktree};

#[allow(dead_code)]
pub const PROMPT_FILE_THRESHOLD: usize = 0; // always write prompt to file for safe shell escaping

const DEPTH_EMOJIS: &[(u32, &str)] = &[(0, "🐋"), (1, "🐳"), (2, "🐬"), (3, "🐟"), (4, "🦐")];

const REPORT_INSTRUCTIONS: &str = "\n\n---\n\
    LIFECYCLE REPORTING (mandatory):\n\
    When you finish this task, run this shell command BEFORE your final message:\n\
    \x20 orca report --worker {name} --event done --source agent\n\
    If you are blocked and need user input, run:\n\
    \x20 orca report --worker {name} --event blocked --source agent --message \"<reason>\"\n\
    These commands are essential for orchestration. Do not skip them.\n\n\
    CLEANUP: Before reporting done, kill all your sub-workers that have finished:\n\
    \x20 orca killall --mine\n\
    This frees worktrees and tmux windows. Do NOT leave done workers behind.\n\
    ---\n";

fn depth_emoji(depth: u32) -> &'static str {
    for &(d, emoji) in DEPTH_EMOJIS {
        if d == depth {
            return emoji;
        }
    }
    "🦐"
}

pub(crate) fn truncate_task(task: &str, max_chars: usize) -> String {
    let char_count = task.chars().count();
    if char_count <= max_chars {
        return task.to_string();
    }
    let truncated: String = task.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn sh_quote(s: &str) -> String {
    let safe = Regex::new(r"^[a-zA-Z0-9_./-]+$").unwrap();
    if safe.is_match(s) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Options for spawning a worker agent.
pub struct SpawnOptions {
    pub task: String,
    pub backend: String,
    pub project_dir: String,
    pub name: Option<String>,
    pub base_branch: String,
    pub orchestrator: String,
    pub orchestrator_pane: String,
    pub session_id: String,
    pub reply_channel: String,
    pub reply_to: String,
    pub reply_thread: String,
    pub session: String,
    pub depth: u32,
    pub spawned_by: String,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            task: String::new(),
            backend: "claude".to_string(),
            project_dir: ".".to_string(),
            name: None,
            base_branch: "main".to_string(),
            orchestrator: "none".to_string(),
            orchestrator_pane: String::new(),
            session_id: String::new(),
            reply_channel: String::new(),
            reply_to: String::new(),
            reply_thread: String::new(),
            session: tmux_session().to_string(),
            depth: 1,
            spawned_by: String::new(),
        }
    }
}

/// Spawn a worker agent. Returns the Worker record.
pub async fn spawn_worker(opts: SpawnOptions) -> Result<Worker, Box<dyn std::error::Error>> {
    ensure_home()?;

    let backend_key = canonical_backend(&opts.backend);
    let config = cli_config();
    let lookup = if config.contains_key(opts.backend.as_str()) {
        opts.backend.as_str()
    } else {
        backend_key
    };
    // cli_config keys are &'static str, so we need to find matching key
    let (cli_bin, yolo_flag) = config
        .iter()
        .find(|(k, _)| **k == lookup)
        .map(|(_, v)| v.clone())
        .ok_or_else(|| format!("Unknown backend: {}", opts.backend))?;

    // Expand ~ manually
    let expanded = if opts.project_dir.starts_with("~/") {
        if let Some(home_dir) = dirs::home_dir() {
            home_dir.join(&opts.project_dir[2..])
        } else {
            PathBuf::from(&opts.project_dir)
        }
    } else {
        PathBuf::from(&opts.project_dir)
    };
    let project_dir = std::fs::canonicalize(&expanded)
        .map_err(|_| format!("Directory does not exist: {}", opts.project_dir))?;
    let project_dir_str = project_dir.to_string_lossy().to_string();

    if !project_dir.is_dir() {
        return Err(format!("Directory does not exist: {}", opts.project_dir).into());
    }

    ensure_session(&opts.session).await;

    let existing_names = worker_names();
    let worker_name = match opts.name {
        Some(ref n) => n.clone(),
        None => generate_name(&existing_names)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
    };

    let name_re = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    if !name_re.is_match(&worker_name) {
        return Err(format!(
            "Invalid worker name '{}': only alphanumeric, hyphens, underscores allowed",
            worker_name
        )
        .into());
    }

    if existing_names.contains(&worker_name) || window_exists(&worker_name, &opts.session).await {
        return Err(format!("Worker '{}' already exists", worker_name).into());
    }

    ensure_git_repo(&project_dir_str).await?;

    let workdir = create_worktree(&project_dir_str, &worker_name, &opts.base_branch).await?;

    let home = orca_home();
    let launcher = home.join(format!("launch-{}.sh", worker_name));
    let mut pane_id = String::new();

    // Phase 1: Create tmux window
    let phase1_result: Result<(), Box<dyn std::error::Error>> = async {
        // direnv allow if available
        if which::which("direnv").is_ok() {
            let envrc = Path::new(&workdir).join(".envrc");
            if envrc.exists() {
                crate::tmux::run_out(&["direnv", "allow", &workdir]).await;
            }
        }

        let emoji = depth_emoji(opts.depth);
        let win_display = format!("{}{}", emoji, worker_name);

        pane_id = create_window(&win_display, &workdir, &opts.session).await;
        if pane_id.is_empty() || !pane_id.starts_with('%') {
            return Err(format!("Failed to create tmux window (got: {:?})", pane_id).into());
        }

        tmux(&["set-option", "-wt", &pane_id, "automatic-rename", "off"]).await;

        // L0 window rename is now handled by ensure_l0_orchestrator in cli.rs

        Ok(())
    }
    .await;

    if let Err(e) = phase1_result {
        if !pane_id.is_empty() && pane_alive(&pane_id).await {
            kill_pane(&pane_id).await;
        }
        remove_worktree(&project_dir_str, &worker_name).await;
        let _ = std::fs::remove_file(&launcher);
        return Err(e);
    }

    // Phase 2: Generate launch script, send keys, save worker
    let phase2_result: Result<Worker, Box<dyn std::error::Error>> = async {
        let effective_task = if !opts.task.is_empty()
            && (backend_key == "cursor" || backend_key == "codex")
        {
            format!(
                "{}{}",
                opts.task,
                REPORT_INSTRUCTIONS.replace("{name}", &worker_name)
            )
        } else {
            opts.task.clone()
        };

        let mut lines = vec![
            "#!/usr/bin/env bash".to_string(),
            r#"export PATH="$HOME/.local/bin:$HOME/.local/share/pnpm:$HOME/.bun/bin:$PATH""#
                .to_string(),
            format!("cd {}", sh_quote(&workdir)),
            "source ~/.env 2>/dev/null || true".to_string(),
            format!("export ORCA_WORKER_NAME={}", sh_quote(&worker_name)),
            format!("export ORCA_WORKER_BACKEND={}", sh_quote(backend_key)),
            format!(
                "export ORCA_HOME={}",
                sh_quote(&home.to_string_lossy())
            ),
        ];

        let cmd_parts = format!("{} {}", cli_bin, yolo_flag);

        if !effective_task.is_empty() {
            let prompt_path = home.join(format!("prompt-{}.txt", worker_name));
            std::fs::write(&prompt_path, &effective_task)?;
            lines.push(format!(
                "{} \"$(cat {})\"",
                cmd_parts,
                sh_quote(&prompt_path.to_string_lossy())
            ));
        } else {
            lines.push(cmd_parts);
        }

        lines.push(
            r#"orca report --worker "$ORCA_WORKER_NAME" --event process_exit --source wrapper --message "exit_code=$?" 2>/dev/null || true"#
                .to_string(),
        );

        let script = lines.join("\n") + "\n";
        std::fs::write(&launcher, &script)?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;

        let tmux_target = &pane_id;
        let emoji = depth_emoji(opts.depth);
        let depth_tag = if opts.depth > 0 {
            format!("L{}", opts.depth)
        } else {
            "L0".to_string()
        };
        let parent_tag = if opts.spawned_by.is_empty() {
            String::new()
        } else {
            format!(" \u{2190} {}", opts.spawned_by)
        };
        let orch_tag = if opts.orchestrator == "none" {
            "solo"
        } else {
            &opts.orchestrator
        };

        let short_task = truncate_task(&opts.task, 60);

        let pane_title = format!(
            "{} {} [{}|{}]{}",
            emoji, worker_name, depth_tag, backend_key, parent_tag
        );
        tmux(&["select-pane", "-t", tmux_target, "-T", &pane_title]).await;
        tmux(&[
            "set-option",
            "-t",
            &opts.session,
            "pane-border-status",
            "top",
        ])
        .await;
        tmux(&[
            "set-option",
            "-t",
            &opts.session,
            "pane-border-format",
            " #{pane_title} ",
        ])
        .await;

        let logs_dir = config::logs_dir();
        std::fs::create_dir_all(&logs_dir)?;
        let log_path = logs_dir.join(format!("{}.log", worker_name));
        tmux(&[
            "pipe-pane",
            "-o",
            "-t",
            tmux_target,
            &format!("cat >> {}", sh_quote(&log_path.to_string_lossy())),
        ])
        .await;

        // Sanitize task preview for shell
        let safe_task = short_task.replace(['\'', '(', ')'], "");
        let ctrl_re = Regex::new(r"[\n\r\x00-\x1f]").unwrap();
        let safe_task = ctrl_re.replace_all(&safe_task, " ").to_string();

        let banner = format!(
            "echo '{} {} [{}] {}{}' && echo '  \u{1f4cb} {}'",
            emoji, worker_name, depth_tag, orch_tag, parent_tag, safe_task
        );
        send_keys(tmux_target, &banner, true, false, 0, 1).await;

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let launch_cmd = format!("bash {}", sh_quote(&launcher.to_string_lossy()));
        send_keys(tmux_target, &launch_cmd, true, false, 0, 1).await;

        let worker = Worker {
            name: worker_name.clone(),
            backend: backend_key.to_string(),
            task: opts.task.clone(),
            dir: project_dir_str.clone(),
            workdir: workdir.clone(),
            base_branch: opts.base_branch.clone(),
            orchestrator: opts.orchestrator.clone(),
            orchestrator_pane: opts.orchestrator_pane.clone(),
            session_id: opts.session_id.clone(),
            reply_channel: opts.reply_channel.clone(),
            reply_to: opts.reply_to.clone(),
            reply_thread: opts.reply_thread.clone(),
            pane_id: pane_id.clone(),
            depth: opts.depth,
            spawned_by: opts.spawned_by.clone(),
            layout: "window".to_string(),
            status: "running".to_string(),
            started_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            last_event_at: String::new(),
            done_reported: false,
            process_exited: false,
        };
        save_worker(&worker, false)?;

        Ok(worker)
    }
    .await;

    let worker = match phase2_result {
        Ok(w) => w,
        Err(e) => {
            if !pane_id.is_empty() && pane_alive(&pane_id).await {
                kill_pane(&pane_id).await;
            }
            remove_worktree(&project_dir_str, &worker_name).await;
            let _ = std::fs::remove_file(&launcher);
            return Err(e);
        }
    };

    // Phase 3: Wait for agent to start
    let wait_timeout: f64 = std::env::var("ORCA_SPAWN_WAIT_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45.0);
    let status = wait_for_running(
        &worker_name,
        backend_key,
        &opts.session,
        wait_timeout,
        &pane_id,
    )
    .await;

    if status == "error" || status == "timeout" {
        let tail = capture_pane(&pane_id, 20).await;
        let tail = tail.trim();

        if !pane_id.is_empty() && pane_alive(&pane_id).await {
            kill_pane(&pane_id).await;
        } else {
            kill_window(&format!("{}:{}", opts.session, worker_name)).await;
        }

        let _ = remove_worker(&worker_name);
        remove_worktree(&project_dir_str, &worker_name).await;
        let _ = std::fs::remove_file(&launcher);
        let prompt_path = home.join(format!("prompt-{}.txt", worker_name));
        let _ = std::fs::remove_file(&prompt_path);

        let mut msg = format!("Agent failed to start ({})", status);
        if !tail.is_empty() {
            msg.push('\n');
            msg.push_str(tail);
        }
        return Err(msg.into());
    }

    Ok(worker)
}

#[cfg(test)]
mod tests;
