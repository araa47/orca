// worktree.rs — Git init + worktree create/remove.

use std::path::Path;
use tokio::time::{Duration, sleep};

use crate::config;
use crate::tmux::run_out;

/// Set a local fallback identity when git user.name/email are unset.
async fn ensure_git_identity(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let checks: &[(&str, &str)] = &[("user.name", "Orca"), ("user.email", "orca@localhost")];
    for &(key, value) in checks {
        let (rc, out, _) = run_out(&["git", "-C", path, "config", "--get", key]).await;
        if rc == 0 && !out.trim().is_empty() {
            continue;
        }
        let (rc, _, stderr) = run_out(&["git", "-C", path, "config", key, value]).await;
        if rc != 0 {
            return Err(format!("git config {key} failed in {path}: {}", stderr.trim()).into());
        }
    }
    Ok(())
}

/// If `path` is not a git repo, initialize one with an initial commit.
pub async fn ensure_git_repo(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (rc, _, _) = run_out(&["git", "-C", path, "rev-parse", "--git-dir"]).await;
    if rc == 0 {
        let (rc, _, _) = run_out(&["git", "-C", path, "rev-parse", "--verify", "HEAD"]).await;
        if rc == 0 {
            return Ok(());
        }
    } else {
        let (rc, _, stderr) = run_out(&["git", "-C", path, "init"]).await;
        if rc != 0 {
            return Err(format!("git init failed in {path}: {}", stderr.trim()).into());
        }
    }

    let (rc, _, stderr) = run_out(&["git", "-C", path, "add", "-A"]).await;
    if rc != 0 {
        return Err(format!("git add failed in {path}: {}", stderr.trim()).into());
    }

    ensure_git_identity(path).await?;

    let (rc, _, stderr) = run_out(&[
        "git",
        "-C",
        path,
        "commit",
        "-m",
        "initial commit",
        "--allow-empty",
    ])
    .await;
    if rc == 0 {
        return Ok(());
    }
    if stderr.to_lowercase().contains("nothing to commit") {
        let (rc, _, _) = run_out(&["git", "-C", path, "rev-parse", "--verify", "HEAD"]).await;
        if rc == 0 {
            return Ok(());
        }
    }
    Err(format!("git initial commit failed in {path}: {}", stderr.trim()).into())
}

/// Create a worktree at `.worktrees/<name>`. Returns the workdir path.
pub async fn create_worktree(
    repo: &str,
    name: &str,
    base_branch: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let wt_dir = Path::new(repo)
        .join(".worktrees")
        .join(name)
        .to_string_lossy()
        .into_owned();
    let wt_rel = format!(".worktrees/{name}");

    // Prune stale worktree refs
    run_out(&["git", "-C", repo, "worktree", "prune"]).await;

    // Remove existing worktree if present
    if Path::new(&wt_dir).is_dir() {
        run_out(&["git", "-C", repo, "worktree", "remove", &wt_rel, "--force"]).await;
        if Path::new(&wt_dir).is_dir() {
            let _ = tokio::fs::remove_dir_all(&wt_dir).await;
            run_out(&["git", "-C", repo, "worktree", "prune"]).await;
        }
    }

    // Fetch with retry on index.lock contention
    let mut fetched_ref = false;
    for attempt in 0..3u64 {
        let (rc, _, stderr) = run_out(&["git", "-C", repo, "fetch", "origin", base_branch]).await;
        if rc == 0 {
            fetched_ref = true;
            break;
        }
        if !stderr.contains("index.lock") {
            break;
        }
        sleep(Duration::from_secs(1 + attempt)).await;
    }

    // Try refs in order: origin/{base_branch}, FETCH_HEAD (if fetched), {base_branch}, HEAD
    let origin_ref = format!("origin/{base_branch}");
    let mut refs: Vec<&str> = vec![origin_ref.as_str()];
    if fetched_ref {
        refs.push("FETCH_HEAD");
    }
    refs.push(base_branch);
    refs.push("HEAD");
    let mut last_error = String::new();

    for git_ref in &refs {
        let (rc, _, _) = run_out(&["git", "-C", repo, "rev-parse", git_ref]).await;
        if rc != 0 {
            continue;
        }

        for attempt in 0..3u64 {
            let (rc, _, stderr) = run_out(&[
                "git", "-C", repo, "worktree", "add", &wt_dir, git_ref, "--detach",
            ])
            .await;
            if rc == 0 {
                return Ok(wt_dir);
            }
            last_error = stderr.trim().to_owned();
            if !stderr.contains("index.lock") {
                break;
            }
            sleep(Duration::from_secs(1 + attempt)).await;
        }
    }

    let refs_display = refs.join(", ");
    let mut msg = format!(
        "Failed to create worktree for '{name}' in {repo} — \
         tried refs: {refs_display}"
    );
    if !last_error.is_empty() {
        msg.push_str(&format!("\nLast error: {last_error}"));
    }
    Err(msg.into())
}

/// If the worktree has uncommitted changes, stash them so they survive removal.
/// Returns `true` if a stash was created.
pub async fn stash_if_dirty(repo: &str, name: &str) -> bool {
    let wt_dir = Path::new(repo)
        .join(".worktrees")
        .join(name)
        .to_string_lossy()
        .into_owned();

    if !Path::new(&wt_dir).is_dir() {
        return false;
    }

    let (rc, status_out, _) = run_out(&["git", "-C", &wt_dir, "status", "--porcelain"]).await;
    if rc != 0 || status_out.trim().is_empty() {
        return false;
    }

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let stash_msg = format!("orca-preserving {name} {ts}");

    let (rc, _, stderr) = run_out(&[
        "git", "-C", &wt_dir, "stash", "push", "-u", "-m", &stash_msg,
    ])
    .await;

    if rc != 0 {
        eprintln!(
            "Warning: git stash failed for worker '{name}': {}",
            stderr.trim()
        );
        return false;
    }

    config::audit(&format!(
        "STASH_PRESERVE worker={name} repo={repo} msg={stash_msg}"
    ));
    eprintln!(
        "Stashed uncommitted changes for '{name}'. Recover from project root:\n  \
         git stash list   # look for \"{stash_msg}\"\n  \
         git stash pop    # or: git stash apply stash@{{n}}"
    );
    true
}

/// Remove worktree `.worktrees/<name>`.
pub async fn remove_worktree(repo: &str, name: &str) {
    let wt_rel = format!(".worktrees/{name}");
    let (rc, _, stderr) =
        run_out(&["git", "-C", repo, "worktree", "remove", &wt_rel, "--force"]).await;
    if rc != 0 && !stderr.to_lowercase().contains("is not a working tree") {
        eprintln!(
            "git worktree remove failed for '{name}' in {repo}: {}",
            stderr.trim()
        );
    }

    let wt_dir = Path::new(repo).join(".worktrees").join(name);
    if wt_dir.is_dir() {
        let _ = tokio::fs::remove_dir_all(&wt_dir).await;
    }

    // Prune so git forgets the worktree ref after manual removal
    run_out(&["git", "-C", repo, "worktree", "prune"]).await;
}

#[cfg(test)]
mod tests;
