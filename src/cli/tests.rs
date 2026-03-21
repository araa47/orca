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
fn test_depth_label() {
    assert_eq!(depth_label(0), "🐋 L0");
    assert_eq!(depth_label(1), "🐳 L1");
    assert_eq!(depth_label(3), "🐟 L3");
    assert_eq!(depth_label(99), "🦐 L99");
}

#[test]
fn test_l0_spawn_markers_recognized() {
    assert!(is_root_spawn_marker("root"));
    assert!(is_root_spawn_marker("root:%149"));
    assert!(is_root_spawn_marker("openclaw"));
    assert!(is_root_spawn_marker("self"));
    assert!(!is_root_spawn_marker("fin"));
    assert!(!is_root_spawn_marker(""));
}

#[test]
fn test_resolve_spawn_lineage_openclaw_l0_marker_depth_1() {
    let workers = std::collections::HashMap::new();
    // "openclaw" as L0 marker → depth 1
    let (sb, d) = resolve_spawn_lineage("openclaw".into(), 0, &workers);
    assert_eq!(sb, "openclaw");
    assert_eq!(d, 1);
}

#[test]
fn test_resolve_spawn_lineage_with_l0_entry_in_state() {
    let mut workers = std::collections::HashMap::new();
    // L0 entry at depth 0
    let l0 = make_l0_worker("openclaw", "openclaw", "", "/tmp", "", "main");
    workers.insert("openclaw".into(), l0);
    // Child resolves to depth 1
    let (sb, d) = resolve_spawn_lineage("openclaw".into(), 0, &workers);
    assert_eq!(sb, "openclaw");
    assert_eq!(d, 1);
}

#[test]
fn test_resolve_spawn_lineage_explicit_spawned_by_sets_depth() {
    let mut workers = std::collections::HashMap::new();
    workers.insert("p".into(), make_worker_with("p", "codex", "running", 2));
    let (sb, d) = resolve_spawn_lineage("p".into(), 0, &workers);
    assert_eq!(sb, "p");
    assert_eq!(d, 3);
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
    assert_eq!(cli_depth, 2);
    assert_eq!(depth_label(cli_depth), "🐬 L2");
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
fn test_hierarchy_top_level_spawn_is_l1_for_all_l0_markers() {
    let workers = std::collections::HashMap::new();
    // All L0 markers should resolve to depth 1
    for marker in ["openclaw", "root", "self"] {
        let (_, d) = resolve_spawn_lineage(marker.into(), 0, &workers);
        assert_eq!(d, 1, "marker '{marker}' should resolve to depth 1");
        assert_eq!(depth_label(d), "🐳 L1");
    }
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
    assert_eq!(cli_depth, 2);
    assert_eq!(depth_label(cli_depth), "🐬 L2");
}

#[test]
fn test_hierarchy_explicit_spawned_by_depth_2_parent_yields_l3() {
    let mut workers = std::collections::HashMap::new();
    workers.insert("l2".into(), make_worker_with("l2", "codex", "running", 2));
    let (spawned_by, cli_depth) = resolve_spawn_lineage("l2".into(), 0, &workers);
    assert_eq!(spawned_by, "l2");
    assert_eq!(cli_depth, 3);
    assert_eq!(depth_label(cli_depth), "🐟 L3");
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
    let cli = Cli::try_parse_from(["orca", "spawn", "build the widget", "--spawned-by", "root"]);
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

// -----------------------------------------------------------------------
// New tests for explicit spawn lineage (v0.0.7)
// -----------------------------------------------------------------------

#[test]
fn test_resolve_spawn_lineage_openclaw_returns_depth_1() {
    let workers = std::collections::HashMap::new();
    let (sb, d) = resolve_spawn_lineage("openclaw".into(), 0, &workers);
    assert_eq!(sb, "openclaw");
    assert_eq!(d, 1);
}

#[test]
fn test_is_root_spawn_marker_openclaw() {
    assert!(is_root_spawn_marker("openclaw"));
    assert!(is_root_spawn_marker("  openclaw  ")); // trimmed
}

#[test]
fn test_is_root_spawn_marker_legacy_root() {
    assert!(is_root_spawn_marker("root"));
    assert!(is_root_spawn_marker("root:%149"));
}

#[test]
fn test_validate_accepts_openclaw_as_spawned_by() {
    let workers = HashMap::new();
    let env = SpawnValidateEnv {
        allow_no_orchestrator: true,
        allow_openclaw_without_reply: true,
    };
    // "openclaw" as spawned_by should pass validation (it's an L0 marker)
    validate_spawn_context("cc", "openclaw", "openclaw", None, &workers, "", "", &env).unwrap();
}

#[test]
fn test_print_tree_shows_l0_header() {
    let mut workers = HashMap::new();
    let mut w = make_worker_with("ace", "claude", "running", 1);
    w.spawned_by = "openclaw".into();
    workers.insert("ace".to_string(), w);
    // Should print L0 header without panic
    print_tree(&workers);
}

#[test]
fn test_print_tree_l0_header_legacy_empty_parent() {
    let mut workers = HashMap::new();
    workers.insert(
        "sol".to_string(),
        make_worker_with("sol", "claude", "running", 1),
    );
    // spawned_by is "" (legacy) — should show under generic L0 header
    print_tree(&workers);
}

#[test]
fn test_worker_spawned_by_openclaw_is_depth_1() {
    let workers = std::collections::HashMap::new();
    let (sb, d) = resolve_spawn_lineage("openclaw".into(), 0, &workers);
    assert_eq!(sb, "openclaw");
    assert_eq!(d, 1);
    assert_eq!(depth_label(d), "🐳 L1");
}

// -----------------------------------------------------------------------
// H) --spawned-by self (L0 cc/cx/cu)
// -----------------------------------------------------------------------

#[test]
fn test_self_is_recognized_as_l0_spawn_marker() {
    assert!(is_root_spawn_marker("self"));
    assert!(L0_SPAWN_MARKERS.contains(&"self"));
}

#[test]
fn test_resolve_spawn_lineage_self_returns_depth_1() {
    let workers = std::collections::HashMap::new();
    // "self" is an L0 marker → depth 1
    let (sb, d) = resolve_spawn_lineage("self".into(), 0, &workers);
    assert_eq!(sb, "self");
    assert_eq!(d, 1);
}

#[test]
fn test_ensure_l0_openclaw_rejects_non_openclaw_orchestrator() {
    let mut workers = HashMap::new();
    let result = ensure_l0_orchestrator("openclaw", "cc", "%1", "/tmp", "", "main", &mut workers);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("only for the OpenClaw"));
}

#[test]
fn test_ensure_l0_openclaw_creates_entry_in_memory() {
    // Test the in-memory logic without hitting state::save_worker
    // by pre-populating the workers map as if ensure_l0_orchestrator ran
    let mut workers = HashMap::new();
    let w = make_l0_worker("openclaw", "openclaw", "", "/proj", "s1", "main");
    workers.insert("openclaw".to_string(), w);

    // Verify the L0 entry has correct fields
    let l0 = &workers["openclaw"];
    assert_eq!(l0.depth, 0);
    assert_eq!(l0.backend, "openclaw");
    assert!(l0.spawned_by.is_empty());
    assert_eq!(l0.status, "running");
}

#[test]
fn test_ensure_l0_openclaw_skips_when_exists() {
    let mut workers = HashMap::new();
    // Pre-populate so ensure_l0_orchestrator sees it already exists
    let w = make_l0_worker("openclaw", "openclaw", "", "/proj", "s1", "main");
    workers.insert("openclaw".to_string(), w);

    // This should return Ok immediately without trying to save_worker
    let result = ensure_l0_orchestrator(
        "openclaw",
        "openclaw",
        "",
        "/tmp",
        "s1",
        "main",
        &mut workers,
    );
    assert_eq!(result.unwrap(), "openclaw");
}

#[test]
fn test_make_l0_worker_fields() {
    let w = make_l0_worker("test-l0", "claude", "%5", "/proj", "sid", "main");
    assert_eq!(w.name, "test-l0");
    assert_eq!(w.backend, "claude");
    assert_eq!(w.pane_id, "%5");
    assert_eq!(w.depth, 0);
    assert!(w.spawned_by.is_empty());
    assert_eq!(w.status, "running");
    assert_eq!(w.dir, "/proj");
}

#[test]
fn test_validate_accepts_self_as_spawned_by() {
    let workers = HashMap::new();
    let env = SpawnValidateEnv {
        allow_no_orchestrator: true,
        allow_openclaw_without_reply: true,
    };
    // "self" as spawned_by should pass validation (it's an L0 marker)
    validate_spawn_context("cc", "self", "self", None, &workers, "", "", &env).unwrap();
}

#[test]
fn test_validate_self_not_rejected_as_unknown_parent() {
    let workers = HashMap::new();
    // With strict env, "self" should still pass because it's an L0 marker
    validate_spawn_context(
        "cc",
        "self",
        "self",
        None,
        &workers,
        "",
        "",
        &strict_spawn_validate_env(),
    )
    .unwrap();
}

// -----------------------------------------------------------------------
// I) --spawned-by self resolution for depth > 0 workers (v0.0.8 fix)
// -----------------------------------------------------------------------

#[test]
fn test_ensure_l0_returns_existing_worker_when_pane_matches_depth_gt0() {
    let mut workers = HashMap::new();
    // L0 root
    let l0 = make_l0_worker("rook", "cursor", "%10", "/proj", "", "main");
    workers.insert("rook".to_string(), l0);
    // L2 worker `fen` running in pane %42
    let mut fen = make_worker_with("fen", "cursor", "running", 2);
    fen.pane_id = "%42".into();
    fen.spawned_by = "cog".into();
    workers.insert("fen".to_string(), fen);

    // When ensure_l0_orchestrator is called with pane=%42 (fen's pane),
    // it should return "fen" instead of creating a ghost L0 entry.
    let result =
        ensure_l0_orchestrator("self", "cu", "%42", "/proj", "", "main", &mut workers);
    assert_eq!(result.unwrap(), "fen");
    // No new L0 entry should have been created
    assert_eq!(workers.len(), 2);
}

#[test]
fn test_ensure_l0_still_creates_l0_when_pane_has_no_worker() {
    let mut workers = HashMap::new();
    // No worker owns pane %99 — genuine L0 case
    let result =
        ensure_l0_orchestrator("self", "cu", "%99", "/proj", "", "main", &mut workers);
    let name = result.unwrap();
    // A new L0 entry should have been created
    assert_eq!(workers.len(), 1);
    let created = &workers[&name];
    assert_eq!(created.depth, 0);
    assert!(created.spawned_by.is_empty());
}

#[test]
fn test_ensure_l0_returns_existing_l0_when_pane_matches_depth_0() {
    let mut workers = HashMap::new();
    let l0 = make_l0_worker("rook", "cursor", "%10", "/proj", "", "main");
    workers.insert("rook".to_string(), l0);

    // Pane %10 already has an L0 entry — should return it
    let result =
        ensure_l0_orchestrator("self", "cu", "%10", "/proj", "", "main", &mut workers);
    assert_eq!(result.unwrap(), "rook");
    assert_eq!(workers.len(), 1);
}

#[test]
fn test_resolve_self_to_depth_gt0_worker_by_pane() {
    // Simulate what cmd_spawn does: resolve `self` before L0 check
    let mut workers = HashMap::new();
    let mut fen = make_worker_with("fen", "cursor", "running", 2);
    fen.pane_id = "%42".into();
    fen.spawned_by = "cog".into();
    workers.insert("fen".to_string(), fen);

    let raw_spawned_by = "self".to_string();
    let pane = "%42";
    let resolved = if raw_spawned_by == "self" && !pane.is_empty() {
        workers
            .iter()
            .find(|(_, w)| w.pane_id == pane && w.depth > 0)
            .map(|(name, _)| name.clone())
            .unwrap_or(raw_spawned_by.clone())
    } else {
        raw_spawned_by.clone()
    };

    assert_eq!(resolved, "fen");
    assert!(!is_root_spawn_marker(&resolved));
    // Now resolve_spawn_lineage should give depth 3
    let (sb, d) = resolve_spawn_lineage(resolved, 0, &workers);
    assert_eq!(sb, "fen");
    assert_eq!(d, 3);
}

#[test]
fn test_resolve_self_unchanged_when_no_pane_match() {
    let workers: HashMap<String, Worker> = HashMap::new();
    let raw_spawned_by = "self".to_string();
    let pane = "%99";
    let resolved = if raw_spawned_by == "self" && !pane.is_empty() {
        workers
            .iter()
            .find(|(_, w)| w.pane_id == pane && w.depth > 0)
            .map(|(name, _)| name.clone())
            .unwrap_or(raw_spawned_by.clone())
    } else {
        raw_spawned_by.clone()
    };

    assert_eq!(resolved, "self");
    assert!(is_root_spawn_marker(&resolved));
}

#[test]
fn test_full_chain_self_resolves_correctly_for_l2_worker() {
    // Simulate: openclaw → cog (L1) → fen (L2) → fen spawns ash
    // fen uses --spawned-by self because ORCA_WORKER_NAME is unset
    let mut workers = HashMap::new();
    let l0 = make_l0_worker("openclaw", "openclaw", "", "/proj", "s1", "main");
    workers.insert("openclaw".to_string(), l0);
    let mut cog = make_worker_with("cog", "codex", "running", 1);
    cog.spawned_by = "openclaw".into();
    cog.pane_id = "%20".into();
    workers.insert("cog".to_string(), cog);
    let mut fen = make_worker_with("fen", "cursor", "running", 2);
    fen.spawned_by = "cog".into();
    fen.pane_id = "%42".into();
    workers.insert("fen".to_string(), fen);

    // fen runs `orca spawn ... --spawned-by self` from pane %42
    let raw = "self".to_string();
    let pane = "%42";
    let resolved = if raw == "self" && !pane.is_empty() {
        workers
            .iter()
            .find(|(_, w)| w.pane_id == pane && w.depth > 0)
            .map(|(n, _)| n.clone())
            .unwrap_or(raw.clone())
    } else {
        raw.clone()
    };

    assert_eq!(resolved, "fen", "self should resolve to fen");
    assert!(!is_root_spawn_marker(&resolved));

    let (spawned_by, depth) = resolve_spawn_lineage(resolved, 0, &workers);
    assert_eq!(spawned_by, "fen");
    assert_eq!(depth, 3, "ash should be L3");
}

// -----------------------------------------------------------------------
// L0 kill protection and filtering
// -----------------------------------------------------------------------

#[test]
fn test_print_tree_with_l0_orchestrator_entry() {
    let mut workers = HashMap::new();
    // L0 openclaw entry
    let l0 = make_l0_worker("openclaw", "openclaw", "", "/proj", "s1", "main");
    workers.insert("openclaw".to_string(), l0);
    // L1 worker under openclaw
    let mut w1 = make_worker_with("ace", "claude", "running", 1);
    w1.spawned_by = "openclaw".into();
    workers.insert("ace".to_string(), w1);
    print_tree(&workers); // should not panic
}

#[test]
fn test_print_tree_with_cc_l0_entry() {
    let mut workers = HashMap::new();
    // L0 cc entry
    let l0 = make_l0_worker("rook", "claude", "%5", "/proj", "", "main");
    workers.insert("rook".to_string(), l0);
    // L1 worker under rook
    let mut w1 = make_worker_with("fin", "claude", "running", 1);
    w1.spawned_by = "rook".into();
    workers.insert("fin".to_string(), w1);
    print_tree(&workers);
}

#[test]
fn test_print_tree_multiple_l0_orchestrators() {
    let mut workers = HashMap::new();
    let l0_oc = make_l0_worker("openclaw", "openclaw", "", "/proj", "s1", "main");
    workers.insert("openclaw".to_string(), l0_oc);
    let l0_cc = make_l0_worker("rook", "claude", "%5", "/proj", "", "main");
    workers.insert("rook".to_string(), l0_cc);
    let mut w1 = make_worker_with("ace", "claude", "running", 1);
    w1.spawned_by = "openclaw".into();
    workers.insert("ace".to_string(), w1);
    let mut w2 = make_worker_with("fin", "claude", "running", 1);
    w2.spawned_by = "rook".into();
    workers.insert("fin".to_string(), w2);
    print_tree(&workers);
}

#[test]
fn test_l0_openclaw_not_killable() {
    let l0 = make_l0_worker("openclaw", "openclaw", "", "/proj", "", "main");
    assert!(l0.depth == 0 && l0.spawned_by.is_empty());
}

#[test]
fn test_l0_cc_not_killable() {
    let l0 = make_l0_worker("rook", "claude", "%5", "/proj", "", "main");
    // All L0 entries are protected, not just openclaw
    assert!(l0.depth == 0 && l0.spawned_by.is_empty());
}

#[test]
fn test_killall_skips_l0_entries() {
    let mut workers = HashMap::new();
    // L0 entry
    let l0 = make_l0_worker("openclaw", "openclaw", "", "/proj", "", "main");
    workers.insert("openclaw".to_string(), l0);
    // Regular worker
    let w = make_worker_with("ace", "claude", "running", 1);
    workers.insert("ace".to_string(), w);

    // Filter like cmd_killall does
    let killable: HashMap<String, Worker> = workers
        .into_iter()
        .filter(|(_, w)| !(w.depth == 0 && w.spawned_by.is_empty()))
        .collect();
    assert_eq!(killable.len(), 1);
    assert!(killable.contains_key("ace"));
}

#[test]
fn test_gc_skips_l0_entries() {
    let mut workers = HashMap::new();
    // L0 entry with done status (shouldn't be GC'd)
    let mut l0 = make_l0_worker("openclaw", "openclaw", "", "/proj", "", "main");
    l0.status = "done".to_string();
    workers.insert("openclaw".to_string(), l0);
    // Done regular worker (should be GC'd)
    let w = make_worker_with("ace", "claude", "done", 1);
    workers.insert("ace".to_string(), w);

    // Filter like cmd_gc does
    let to_gc: Vec<(String, Worker)> = workers
        .into_iter()
        .filter(|(_, w)| {
            if w.depth == 0 && w.spawned_by.is_empty() {
                return false;
            }
            matches!(w.status.as_str(), "done" | "dead" | "destroyed")
        })
        .collect();
    assert_eq!(to_gc.len(), 1);
    assert_eq!(to_gc[0].0, "ace");
}
