use super::*;
use crate::types::{Backend, Orchestrator, WorkerStatus};

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
        backend: Backend::Claude,
        task: "old task".into(),
        dir: dir.clone(),
        workdir: dir.clone(),
        base_branch: "main".into(),
        orchestrator: Orchestrator::None,
        orchestrator_pane: String::new(),
        session_id: String::new(),
        reply_channel: String::new(),
        reply_to: String::new(),
        reply_thread: String::new(),
        pane_id: "%1".into(),
        depth: 0,
        spawned_by: String::new(),
        layout: "window".into(),
        status: WorkerStatus::Running,
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
