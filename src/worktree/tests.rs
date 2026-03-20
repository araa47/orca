use super::*;

/// Create a fresh temp dir and return the path string.
fn make_temp_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

/// Create a temp dir that is already a git repo with an initial commit.
async fn make_git_repo() -> (tempfile::TempDir, String) {
    let (dir, path) = make_temp_dir();
    run_out(&["git", "-C", &path, "init"]).await;
    run_out(&["git", "-C", &path, "config", "user.name", "Test"]).await;
    run_out(&["git", "-C", &path, "config", "user.email", "test@test.com"]).await;
    // Create a file so there's something to commit
    std::fs::write(Path::new(&path).join("README.md"), "# test").unwrap();
    run_out(&["git", "-C", &path, "add", "-A"]).await;
    run_out(&["git", "-C", &path, "commit", "-m", "init", "--allow-empty"]).await;
    (dir, path)
}

// -----------------------------------------------------------------------
// ensure_git_repo
// -----------------------------------------------------------------------

#[tokio::test]
async fn ensure_git_repo_initializes_empty_dir() {
    let (_dir, path) = make_temp_dir();
    let result = ensure_git_repo(&path).await;
    assert!(result.is_ok(), "ensure_git_repo failed: {:?}", result);

    // Verify it's now a git repo with HEAD
    let (rc, _, _) = run_out(&["git", "-C", &path, "rev-parse", "--verify", "HEAD"]).await;
    assert_eq!(rc, 0, "HEAD should exist after ensure_git_repo");
}

#[tokio::test]
async fn ensure_git_repo_idempotent_on_existing_repo() {
    let (_dir, path) = make_git_repo().await;

    // Get the current HEAD
    let (_, head_before, _) = run_out(&["git", "-C", &path, "rev-parse", "HEAD"]).await;

    let result = ensure_git_repo(&path).await;
    assert!(result.is_ok());

    // HEAD should not have changed
    let (_, head_after, _) = run_out(&["git", "-C", &path, "rev-parse", "HEAD"]).await;
    assert_eq!(head_before.trim(), head_after.trim());
}

#[tokio::test]
async fn ensure_git_repo_handles_init_but_no_commit() {
    let (_dir, path) = make_temp_dir();
    // Init but don't commit
    run_out(&["git", "-C", &path, "init"]).await;

    let result = ensure_git_repo(&path).await;
    assert!(
        result.is_ok(),
        "should handle git-inited-but-no-commit: {:?}",
        result
    );

    let (rc, _, _) = run_out(&["git", "-C", &path, "rev-parse", "--verify", "HEAD"]).await;
    assert_eq!(rc, 0);
}

#[tokio::test]
async fn ensure_git_repo_sets_identity_if_missing() {
    let (_dir, path) = make_temp_dir();
    let result = ensure_git_repo(&path).await;
    assert!(result.is_ok());

    let (rc, name, _) = run_out(&["git", "-C", &path, "config", "--get", "user.name"]).await;
    assert_eq!(rc, 0);
    assert!(!name.trim().is_empty());
}

#[tokio::test]
async fn ensure_git_repo_stages_existing_files() {
    let (_dir, path) = make_temp_dir();
    std::fs::write(Path::new(&path).join("hello.txt"), "world").unwrap();

    let result = ensure_git_repo(&path).await;
    assert!(result.is_ok());

    // Verify file was committed
    let (rc, out, _) = run_out(&["git", "-C", &path, "log", "--oneline", "--all"]).await;
    assert_eq!(rc, 0);
    assert!(!out.trim().is_empty());
}

// -----------------------------------------------------------------------
// ensure_git_identity
// -----------------------------------------------------------------------

#[tokio::test]
async fn ensure_git_identity_preserves_existing() {
    let (_dir, path) = make_temp_dir();
    run_out(&["git", "-C", &path, "init"]).await;
    run_out(&["git", "-C", &path, "config", "user.name", "Custom Name"]).await;
    run_out(&[
        "git",
        "-C",
        &path,
        "config",
        "user.email",
        "custom@example.com",
    ])
    .await;

    let result = ensure_git_identity(&path).await;
    assert!(result.is_ok());

    let (_, name, _) = run_out(&["git", "-C", &path, "config", "--get", "user.name"]).await;
    assert_eq!(name.trim(), "Custom Name");
    let (_, email, _) = run_out(&["git", "-C", &path, "config", "--get", "user.email"]).await;
    assert_eq!(email.trim(), "custom@example.com");
}

#[tokio::test]
async fn ensure_git_identity_sets_defaults_when_no_global() {
    let (_dir, path) = make_temp_dir();
    run_out(&["git", "-C", &path, "init"]).await;

    let result = ensure_git_identity(&path).await;
    assert!(result.is_ok());

    // After ensure_git_identity, local config should exist (either from
    // global or set by the function). Just verify it doesn't error.
    let (rc, name, _) = run_out(&["git", "-C", &path, "config", "--get", "user.name"]).await;
    assert_eq!(rc, 0);
    assert!(!name.trim().is_empty(), "user.name should be set");

    let (rc, email, _) = run_out(&["git", "-C", &path, "config", "--get", "user.email"]).await;
    assert_eq!(rc, 0);
    assert!(!email.trim().is_empty(), "user.email should be set");
}

// -----------------------------------------------------------------------
// create_worktree
// -----------------------------------------------------------------------

#[tokio::test]
async fn create_worktree_success() {
    let (_dir, path) = make_git_repo().await;

    let result = create_worktree(&path, "test-worker", "HEAD").await;
    assert!(result.is_ok(), "create_worktree failed: {:?}", result);

    let wt_path = result.unwrap();
    assert!(
        Path::new(&wt_path).is_dir(),
        "worktree dir should exist at {wt_path}"
    );
    assert!(wt_path.contains(".worktrees/test-worker"));
}

#[tokio::test]
async fn create_worktree_falls_back_to_head() {
    let (_dir, path) = make_git_repo().await;

    // No origin/main exists, so it should fall back to HEAD
    let result = create_worktree(&path, "fallback-worker", "main").await;
    assert!(result.is_ok(), "should fall back to HEAD: {:?}", result);
}

#[tokio::test]
async fn create_worktree_replaces_existing() {
    let (_dir, path) = make_git_repo().await;

    let first = create_worktree(&path, "replace-worker", "HEAD").await;
    assert!(first.is_ok());

    // Create again — should succeed by removing old one first
    let second = create_worktree(&path, "replace-worker", "HEAD").await;
    assert!(
        second.is_ok(),
        "should replace existing worktree: {:?}",
        second
    );
}

#[tokio::test]
async fn create_worktree_multiple_workers() {
    let (_dir, path) = make_git_repo().await;

    let r1 = create_worktree(&path, "worker-a", "HEAD").await;
    let r2 = create_worktree(&path, "worker-b", "HEAD").await;
    assert!(r1.is_ok());
    assert!(r2.is_ok());
    assert_ne!(r1.unwrap(), r2.unwrap());
}

// -----------------------------------------------------------------------
// remove_worktree
// -----------------------------------------------------------------------

#[tokio::test]
async fn remove_worktree_after_create() {
    let (_dir, path) = make_git_repo().await;

    let wt = create_worktree(&path, "rm-worker", "HEAD").await.unwrap();
    assert!(Path::new(&wt).is_dir());

    remove_worktree(&path, "rm-worker").await;
    assert!(!Path::new(&wt).is_dir(), "worktree dir should be removed");
}

#[tokio::test]
async fn remove_worktree_nonexistent_does_not_panic() {
    let (_dir, path) = make_git_repo().await;
    remove_worktree(&path, "nonexistent-worker").await;
}

#[tokio::test]
async fn remove_worktree_on_non_repo_does_not_panic() {
    let (_dir, path) = make_temp_dir();
    remove_worktree(&path, "any-worker").await;
}

#[tokio::test]
async fn remove_worktree_idempotent() {
    let (_dir, path) = make_git_repo().await;
    create_worktree(&path, "idem-worker", "HEAD").await.unwrap();

    remove_worktree(&path, "idem-worker").await;
    remove_worktree(&path, "idem-worker").await;
}

// -----------------------------------------------------------------------
// Full lifecycle: init -> worktree -> remove
// -----------------------------------------------------------------------

#[tokio::test]
async fn full_lifecycle() {
    let (_dir, path) = make_temp_dir();

    // 1. ensure_git_repo on empty dir
    ensure_git_repo(&path).await.unwrap();

    // 2. create a worktree
    let wt = create_worktree(&path, "lifecycle-w", "HEAD").await.unwrap();
    assert!(Path::new(&wt).is_dir());

    // 3. Verify the worktree is a git checkout
    let (rc, _, _) = run_out(&["git", "-C", &wt, "rev-parse", "--git-dir"]).await;
    assert_eq!(rc, 0, "worktree should be a valid git checkout");

    // 4. remove
    remove_worktree(&path, "lifecycle-w").await;
    assert!(!Path::new(&wt).is_dir());
}

// -----------------------------------------------------------------------
// Error paths
// -----------------------------------------------------------------------

#[tokio::test]
async fn ensure_git_repo_invalid_path() {
    let result = ensure_git_repo("/nonexistent/path/__orca_test__").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn create_worktree_invalid_repo_path() {
    let result = create_worktree("/nonexistent/__orca__", "worker", "HEAD").await;
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// stash_if_dirty
// -----------------------------------------------------------------------

#[tokio::test]
async fn stash_if_dirty_on_clean_worktree_returns_false() {
    let (_dir, path) = make_git_repo().await;
    let wt = create_worktree(&path, "clean-stash", "HEAD").await.unwrap();
    assert!(Path::new(&wt).is_dir());

    let stashed = stash_if_dirty(&path, "clean-stash").await;
    assert!(!stashed, "clean worktree should not produce a stash");

    remove_worktree(&path, "clean-stash").await;
}

#[tokio::test]
async fn stash_if_dirty_on_dirty_worktree_returns_true() {
    let (_dir, path) = make_git_repo().await;
    let wt = create_worktree(&path, "dirty-stash", "HEAD").await.unwrap();

    std::fs::write(Path::new(&wt).join("new-file.txt"), "uncommitted").unwrap();

    let stashed = stash_if_dirty(&path, "dirty-stash").await;
    assert!(stashed, "dirty worktree should produce a stash");

    let (rc, stash_list, _) = run_out(&["git", "-C", &path, "stash", "list"]).await;
    assert_eq!(rc, 0);
    assert!(
        stash_list.contains("orca-preserving dirty-stash"),
        "stash message should contain worker name, got: {stash_list}"
    );

    remove_worktree(&path, "dirty-stash").await;
}

#[tokio::test]
async fn stash_if_dirty_nonexistent_worktree_returns_false() {
    let (_dir, path) = make_git_repo().await;
    let stashed = stash_if_dirty(&path, "does-not-exist").await;
    assert!(!stashed);
}

#[tokio::test]
async fn stash_if_dirty_with_modified_tracked_file() {
    let (_dir, path) = make_git_repo().await;
    let wt = create_worktree(&path, "mod-stash", "HEAD").await.unwrap();

    let readme = Path::new(&wt).join("README.md");
    std::fs::write(&readme, "modified content").unwrap();

    let stashed = stash_if_dirty(&path, "mod-stash").await;
    assert!(stashed, "modified tracked file should produce a stash");

    remove_worktree(&path, "mod-stash").await;
}

#[tokio::test]
async fn remove_worktree_after_stash_preserves_stash() {
    let (_dir, path) = make_git_repo().await;
    let wt = create_worktree(&path, "preserve-stash", "HEAD")
        .await
        .unwrap();

    std::fs::write(Path::new(&wt).join("important.txt"), "do not lose").unwrap();

    stash_if_dirty(&path, "preserve-stash").await;
    remove_worktree(&path, "preserve-stash").await;

    let (rc, stash_list, _) = run_out(&["git", "-C", &path, "stash", "list"]).await;
    assert_eq!(rc, 0);
    assert!(
        stash_list.contains("orca-preserving preserve-stash"),
        "stash should survive worktree removal"
    );
}

// -----------------------------------------------------------------------
// ensure_git_repo — existing HEAD with no files to add
// -----------------------------------------------------------------------

#[tokio::test]
async fn ensure_git_repo_existing_head_no_new_files() {
    let (_dir, path) = make_git_repo().await;

    // Repo already has HEAD and no untracked files
    let result = ensure_git_repo(&path).await;
    assert!(
        result.is_ok(),
        "existing repo with HEAD and no new files should succeed: {:?}",
        result
    );

    // Verify HEAD is unchanged
    let (rc, _, _) = run_out(&["git", "-C", &path, "rev-parse", "--verify", "HEAD"]).await;
    assert_eq!(rc, 0);

    // Verify only one commit exists (no extra commit created)
    let (_, log_out, _) = run_out(&["git", "-C", &path, "log", "--oneline"]).await;
    let commit_count = log_out.trim().lines().count();
    assert_eq!(
        commit_count, 1,
        "should still have only 1 commit, got {commit_count}"
    );
}

// -----------------------------------------------------------------------
// create_worktree — error when all refs fail
// -----------------------------------------------------------------------

#[tokio::test]
async fn create_worktree_all_refs_fail() {
    let (bad_dir, bad_path) = make_temp_dir();
    // Init but create a broken state: git dir exists but no valid refs
    run_out(&["git", "-C", &bad_path, "init"]).await;
    // Remove the HEAD file to break the repo
    let head_path = Path::new(&bad_path).join(".git/HEAD");
    let _ = std::fs::remove_file(&head_path);

    let result = create_worktree(&bad_path, "fail-worker", "nonexistent-branch").await;
    assert!(result.is_err(), "should fail when all refs are invalid");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Failed to create worktree"),
        "error should mention worktree failure: {err}"
    );
    assert!(
        err.contains("tried refs"),
        "error should list tried refs: {err}"
    );
    drop(bad_dir);
}

// -----------------------------------------------------------------------
// ensure_git_identity — preserves existing config values
// -----------------------------------------------------------------------

#[tokio::test]
async fn ensure_git_identity_preserves_custom_name_only() {
    let (_dir, path) = make_temp_dir();
    run_out(&["git", "-C", &path, "init"]).await;
    // Set only user.name, leave user.email unset
    run_out(&["git", "-C", &path, "config", "user.name", "My Custom Name"]).await;

    let result = ensure_git_identity(&path).await;
    assert!(result.is_ok());

    let (_, name, _) = run_out(&["git", "-C", &path, "config", "--get", "user.name"]).await;
    assert_eq!(
        name.trim(),
        "My Custom Name",
        "custom user.name should be preserved"
    );

    let (rc, email, _) = run_out(&["git", "-C", &path, "config", "--get", "user.email"]).await;
    assert_eq!(rc, 0);
    assert!(
        !email.trim().is_empty(),
        "user.email should be set (either global or fallback)"
    );
}

#[tokio::test]
async fn ensure_git_identity_preserves_custom_email_only() {
    let (_dir, path) = make_temp_dir();
    run_out(&["git", "-C", &path, "init"]).await;
    // Set only user.email, leave user.name unset
    run_out(&[
        "git",
        "-C",
        &path,
        "config",
        "user.email",
        "custom@example.org",
    ])
    .await;

    let result = ensure_git_identity(&path).await;
    assert!(result.is_ok());

    let (_, email, _) = run_out(&["git", "-C", &path, "config", "--get", "user.email"]).await;
    assert_eq!(
        email.trim(),
        "custom@example.org",
        "custom user.email should be preserved"
    );

    let (rc, name, _) = run_out(&["git", "-C", &path, "config", "--get", "user.name"]).await;
    assert_eq!(rc, 0);
    assert!(
        !name.trim().is_empty(),
        "user.name should be set (either global or fallback)"
    );
}

// -----------------------------------------------------------------------
// create_worktree — nonexistent branch falls back through ref chain
// -----------------------------------------------------------------------

#[tokio::test]
async fn create_worktree_nonexistent_branch_falls_back() {
    let (_dir, path) = make_git_repo().await;

    // "nonexistent-branch" doesn't exist, should fall through to HEAD
    let result = create_worktree(&path, "fallback-ref-worker", "nonexistent-branch").await;
    assert!(
        result.is_ok(),
        "should fall back to HEAD when branch doesn't exist: {:?}",
        result
    );

    let wt = result.unwrap();
    assert!(Path::new(&wt).is_dir());

    // Clean up
    remove_worktree(&path, "fallback-ref-worker").await;
}

// -----------------------------------------------------------------------
// ensure_git_repo — git-inited dir with existing files but no commit
// -----------------------------------------------------------------------

#[tokio::test]
async fn ensure_git_repo_inited_with_files_no_commit() {
    let (_dir, path) = make_temp_dir();
    run_out(&["git", "-C", &path, "init"]).await;
    // Add files but don't commit
    std::fs::write(Path::new(&path).join("file1.txt"), "content1").unwrap();
    std::fs::write(Path::new(&path).join("file2.txt"), "content2").unwrap();

    let result = ensure_git_repo(&path).await;
    assert!(result.is_ok(), "should commit existing files: {:?}", result);

    // Verify files are committed
    let (rc, show_out, _) =
        run_out(&["git", "-C", &path, "show", "--name-only", "--format="]).await;
    assert_eq!(rc, 0);
    assert!(show_out.contains("file1.txt"));
    assert!(show_out.contains("file2.txt"));
}
