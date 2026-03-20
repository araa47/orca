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
mod tests {
    use super::*;

    fn setup_spawn_env(tmp_home: &tempfile::TempDir) {
        unsafe {
            std::env::set_var("ORCA_HOME", tmp_home.path().to_str().unwrap());
            std::env::set_var("ORCA_SPAWN_WAIT_TIMEOUT", "1");
        }
        let _ = crate::config::ensure_home();
    }

    #[test]
    fn test_sh_quote_safe_strings() {
        assert_eq!(sh_quote("hello"), "hello");
        assert_eq!(sh_quote("path/to/file.txt"), "path/to/file.txt");
        assert_eq!(sh_quote("a-b_c.d"), "a-b_c.d");
        assert_eq!(sh_quote("/usr/bin/env"), "/usr/bin/env");
    }

    #[test]
    fn test_sh_quote_unsafe_strings() {
        assert_eq!(sh_quote("hello world"), "'hello world'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("foo;bar"), "'foo;bar'");
        assert_eq!(sh_quote("$(cmd)"), "'$(cmd)'");
        assert_eq!(sh_quote(""), "''");
    }

    // -----------------------------------------------------------------------
    // truncate_task
    // -----------------------------------------------------------------------

    #[test]
    fn test_truncate_task_short_string() {
        assert_eq!(truncate_task("hello", 60), "hello");
    }

    #[test]
    fn test_truncate_task_exact_limit() {
        let task = "A".repeat(60);
        assert_eq!(truncate_task(&task, 60), task);
    }

    #[test]
    fn test_truncate_task_long_ascii() {
        let task = "A".repeat(100);
        let result = truncate_task(&task, 60);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 61); // 60 chars + ellipsis
    }

    #[test]
    fn test_truncate_task_multibyte_under_limit() {
        // 20 emojis = 80 bytes but only 20 chars — under the 60-char limit
        let task = "🐋".repeat(20);
        assert_eq!(truncate_task(&task, 60), task);
    }

    #[test]
    fn test_truncate_task_multibyte_over_limit() {
        // 61 emojis = 244 bytes, 61 chars — over the 60-char limit
        // Previously this would panic with byte-based slicing
        let task = "🐋".repeat(61);
        let result = truncate_task(&task, 60);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 61); // 60 emoji + ellipsis
    }

    #[test]
    fn test_truncate_task_mixed_ascii_and_multibyte() {
        // Mix of ASCII and multi-byte to verify char-based counting
        let task = format!("{}{}", "A".repeat(50), "🐋".repeat(20));
        let result = truncate_task(&task, 60);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 61);
        assert!(result.starts_with("AAAA"));
    }

    #[test]
    fn test_truncate_task_empty() {
        assert_eq!(truncate_task("", 60), "");
    }

    #[test]
    fn test_depth_emoji_known() {
        assert_eq!(depth_emoji(0), "🐋");
        assert_eq!(depth_emoji(1), "🐳");
        assert_eq!(depth_emoji(2), "🐬");
        assert_eq!(depth_emoji(3), "🐟");
        assert_eq!(depth_emoji(4), "🦐");
    }

    #[test]
    fn test_depth_emoji_out_of_range() {
        assert_eq!(depth_emoji(5), "🦐");
        assert_eq!(depth_emoji(99), "🦐");
    }

    #[test]
    fn test_prompt_file_threshold() {
        assert_eq!(PROMPT_FILE_THRESHOLD, 0);
    }

    #[test]
    fn test_spawn_options_defaults() {
        let opts = SpawnOptions::default();
        assert_eq!(opts.task, "");
        assert_eq!(opts.backend, "claude");
        assert_eq!(opts.project_dir, ".");
        assert!(opts.name.is_none());
        assert_eq!(opts.base_branch, "main");
        assert_eq!(opts.orchestrator, "none");
        assert_eq!(opts.orchestrator_pane, "");
        assert_eq!(opts.session_id, "");
        assert_eq!(opts.reply_channel, "");
        assert_eq!(opts.reply_to, "");
        assert_eq!(opts.reply_thread, "");
        assert_eq!(opts.depth, 1);
        assert_eq!(opts.spawned_by, "");
    }

    #[test]
    fn test_report_instructions_contains_placeholders() {
        assert!(REPORT_INSTRUCTIONS.contains("{name}"));
        assert!(REPORT_INSTRUCTIONS.contains("orca report"));
        assert!(REPORT_INSTRUCTIONS.contains("done"));
        assert!(REPORT_INSTRUCTIONS.contains("blocked"));
    }

    #[test]
    fn test_report_instructions_replacement() {
        let result = REPORT_INSTRUCTIONS.replace("{name}", "fox");
        assert!(result.contains("--worker fox"));
        assert!(!result.contains("{name}"));
    }

    // -----------------------------------------------------------------------
    // sh_quote — additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_sh_quote_with_spaces_and_quotes() {
        let result = sh_quote("it's a \"test\"");
        assert!(result.starts_with('\''));
        assert!(result.ends_with('\''));
    }

    #[test]
    fn test_sh_quote_with_backticks() {
        let result = sh_quote("`cmd`");
        assert_eq!(result, "'`cmd`'");
    }

    #[test]
    fn test_sh_quote_with_newlines() {
        let result = sh_quote("line1\nline2");
        assert!(result.starts_with('\''));
    }

    #[test]
    fn test_sh_quote_with_equals() {
        let result = sh_quote("KEY=VALUE");
        assert!(result.starts_with('\''));
    }

    #[test]
    fn test_sh_quote_dot_slash() {
        assert_eq!(sh_quote("./run"), "./run");
    }

    // -----------------------------------------------------------------------
    // depth_emoji — edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_depth_emoji_boundary() {
        assert_eq!(depth_emoji(4), "🦐");
        assert_eq!(depth_emoji(5), "🦐");
    }

    #[test]
    fn test_depth_emoji_large_value() {
        assert_eq!(depth_emoji(u32::MAX), "🦐");
    }

    // -----------------------------------------------------------------------
    // SpawnOptions — field checks
    // -----------------------------------------------------------------------

    #[test]
    fn test_spawn_options_custom() {
        let opts = SpawnOptions {
            task: "do something".into(),
            backend: "codex".into(),
            project_dir: "/tmp/proj".into(),
            name: Some("fox".into()),
            depth: 2,
            spawned_by: "parent".into(),
            ..Default::default()
        };
        assert_eq!(opts.task, "do something");
        assert_eq!(opts.backend, "codex");
        assert_eq!(opts.project_dir, "/tmp/proj");
        assert_eq!(opts.name, Some("fox".into()));
        assert_eq!(opts.depth, 2);
        assert_eq!(opts.spawned_by, "parent");
        assert_eq!(opts.base_branch, "main");
    }

    // -----------------------------------------------------------------------
    // spawn_worker — error paths (async)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_invalid_backend() {
        let opts = SpawnOptions {
            backend: "nonexistent_backend_xyz".into(),
            project_dir: "/tmp".into(),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Unknown backend"),
            "expected 'Unknown backend' in: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_nonexistent_directory() {
        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: "/tmp/__orca_nonexistent_dir_xyz__".into(),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist' in: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_invalid_worker_name() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: dir.path().to_string_lossy().into_owned(),
            name: Some("invalid name with spaces!!".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Invalid worker name"),
            "expected 'Invalid worker name' in: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_invalid_name_special_chars() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: dir.path().to_string_lossy().into_owned(),
            name: Some("worker@#$".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Invalid worker name"));
    }

    #[tokio::test]
    async fn spawn_worker_tilde_expansion_nonexistent() {
        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: "~/__orca_nonexistent_dir_xyz__".into(),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist' in: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_worker — empty task with various backends
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_empty_task_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: String::new(),
            backend: "claude".into(),
            project_dir: dir,
            name: Some("empty-task-claude".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        // Will fail at create_window (no tmux), but should get past validation
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend") && !err.contains("does not exist"),
            "should pass validation, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_empty_task_codex() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: String::new(),
            backend: "codex".into(),
            project_dir: dir,
            name: Some("empty-task-codex".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend"),
            "codex should be a valid backend, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_empty_task_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: String::new(),
            backend: "cursor".into(),
            project_dir: dir,
            name: Some("empty-task-cursor".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend"),
            "cursor should be a valid backend, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_worker — relative dir "." with git repo
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_relative_dir_reaches_tmux_step() {
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        // "." resolves to the current dir which has a git repo (this project)
        let opts = SpawnOptions {
            task: "do something".into(),
            backend: "claude".into(),
            project_dir: ".".into(),
            name: Some("rel-dir-test".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        // Fails at tmux create_window, but covers all pre-tmux code
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend")
                && !err.contains("does not exist")
                && !err.contains("Invalid worker name"),
            "should pass all validation, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_worker — duplicate name (already in state)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_duplicate_name_in_state() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        // Pre-save a worker with the target name
        let existing = crate::state::Worker {
            name: "dup-name".into(),
            backend: "claude".into(),
            task: "old task".into(),
            dir: dir.clone(),
            workdir: dir.clone(),
            base_branch: "main".into(),
            orchestrator: "none".into(),
            orchestrator_pane: String::new(),
            session_id: String::new(),
            reply_channel: String::new(),
            reply_to: String::new(),
            reply_thread: String::new(),
            pane_id: "%1".into(),
            depth: 0,
            spawned_by: String::new(),
            layout: "window".into(),
            status: "running".into(),
            started_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            last_event_at: String::new(),
            done_reported: false,
            process_exited: false,
        };
        crate::state::save_worker(&existing, true).unwrap();

        let opts = SpawnOptions {
            task: "new task".into(),
            backend: "claude".into(),
            project_dir: dir,
            name: Some("dup-name".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("already exists"),
            "expected 'already exists' in: {err}"
        );

        let _ = crate::state::remove_worker("dup-name");
    }

    // -----------------------------------------------------------------------
    // spawn_worker — long task (>60 chars) tests short_task truncation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_long_task_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let long_task = "A".repeat(200);
        let opts = SpawnOptions {
            task: long_task,
            backend: "claude".into(),
            project_dir: dir,
            name: Some("long-task-test".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        // Should fail at tmux, but exercises the short_task truncation path
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend") && !err.contains("Invalid worker name"),
            "should pass validation, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_worker — backend lookup: known aliases
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_backend_alias_cc() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: "test".into(),
            backend: "cc".into(),
            project_dir: dir,
            name: Some("alias-cc-test".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend"),
            "cc alias should resolve, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_backend_alias_cx() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: "test".into(),
            backend: "cx".into(),
            project_dir: dir,
            name: Some("alias-cx-test".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend"),
            "cx alias should resolve, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_backend_alias_cu() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: "test".into(),
            backend: "cu".into(),
            project_dir: dir,
            name: Some("alias-cu-test".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend"),
            "cu alias should resolve, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_worker — worker name regex validation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_valid_name_alphanumeric() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: dir,
            name: Some("valid123".into()),
            task: "test".into(),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Invalid worker name"),
            "alphanumeric name should be valid, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_valid_name_with_hyphens_underscores() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: dir,
            name: Some("my-worker_01".into()),
            task: "test".into(),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Invalid worker name"),
            "hyphen/underscore name should be valid, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_invalid_name_dot() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: dir.path().to_string_lossy().into_owned(),
            name: Some("worker.name".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Invalid worker name"),
            "dot in name should be invalid, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_invalid_name_slash() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: dir.path().to_string_lossy().into_owned(),
            name: Some("worker/name".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Invalid worker name"),
            "slash in name should be invalid, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_worker_invalid_name_emoji() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SpawnOptions {
            backend: "claude".into(),
            project_dir: dir.path().to_string_lossy().into_owned(),
            name: Some("🐋worker".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Invalid worker name"),
            "emoji in name should be invalid, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_worker — codex/cursor backend appends REPORT_INSTRUCTIONS
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_codex_backend_with_task() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: "implement feature X".into(),
            backend: "codex".into(),
            project_dir: dir,
            name: Some("codex-report-test".into()),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        // Will fail at tmux step but exercises the REPORT_INSTRUCTIONS append path
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("Unknown backend") && !err.contains("Invalid worker name"),
            "should pass validation for codex with task, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // spawn_worker — depth 0 and spawned_by tags
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn spawn_worker_depth_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: "orchestrate".into(),
            backend: "claude".into(),
            project_dir: dir,
            name: Some("depth0-test".into()),
            depth: 0,
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn spawn_worker_with_spawned_by() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_home = tempfile::tempdir().unwrap();
        setup_spawn_env(&tmp_home);

        let dir = tmp.path().to_string_lossy().into_owned();
        ensure_git_repo(&dir).await.unwrap();

        let opts = SpawnOptions {
            task: "sub task".into(),
            backend: "claude".into(),
            project_dir: dir,
            name: Some("spawned-by-test".into()),
            depth: 2,
            spawned_by: "parent-worker".into(),
            ..Default::default()
        };
        let result = spawn_worker(opts).await;
        assert!(result.is_err());
    }
}
