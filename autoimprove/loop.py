#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "subprocess-tee>=0.4",
# ]
# ///
"""
Orca auto-improvement loop.

Runs CI, identifies issues, spawns a worker (via orca or direct claude) to fix
them, validates, keeps or discards. The main process is the orchestrator; with
--use-orca it spawns one orca worker per iteration (worktree), then merges and
validates.

Usage:
    uv run autoimprove/loop.py                    # use program.md defaults (direct claude)
    uv run autoimprove/loop.py --use-orca         # orchestrate via orca spawn per iteration
    uv run autoimprove/loop.py --use-orca --sprint-team   # inject sprint-team coder role
    uv run autoimprove/loop.py --max-iters 5     # stop after 5 iterations
    uv run autoimprove/loop.py --dry-run         # show what would run (default 3 iters)
"""

import argparse
import datetime
import os
import shutil
import subprocess
import sys
import textwrap
import time
from dataclasses import dataclass, field
from pathlib import Path

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

PROJECT_DIR = Path(__file__).resolve().parent.parent
RESULTS_FILE = PROJECT_DIR / "autoimprove" / "results.tsv"
RUN_LOG = PROJECT_DIR / "autoimprove" / "run.log"
PROGRAM_FILE = PROJECT_DIR / "autoimprove" / "program.md"
SPRINT_TEAM_REFERENCES = PROJECT_DIR / ".agents" / "skills" / "sprint-team" / "references"

CI_COMMANDS = [
    ("fmt", ["cargo", "fmt", "--check"]),
    ("clippy", ["cargo", "clippy", "--", "-D", "warnings"]),
    ("test", ["cargo", "test"]),
]

# How long to let Claude Code work on a single fix (seconds)
CLAUDE_TIMEOUT = 300

# When using orca: poll interval (seconds) and max wait per iteration
ORCA_POLL_INTERVAL = 15
ORCA_WAIT_TIMEOUT = 600  # 10 min per iteration

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


@dataclass
class CIResult:
    name: str
    passed: bool
    output: str
    duration: float


@dataclass
class Iteration:
    number: int
    timestamp: str = ""
    commit_before: str = ""
    commit_after: str = ""
    ci_before: list[CIResult] = field(default_factory=list)
    ci_after: list[CIResult] = field(default_factory=list)
    issue_summary: str = ""
    fix_description: str = ""
    status: str = ""  # keep, discard, skip


def sh(cmd: list[str], timeout: int = 120, cwd: Path | None = None) -> subprocess.CompletedProcess:
    """Run a shell command, return CompletedProcess."""
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=cwd or PROJECT_DIR,
    )


def git_short_hash() -> str:
    r = sh(["git", "rev-parse", "--short", "HEAD"])
    return r.stdout.strip()


def git_branch() -> str:
    r = sh(["git", "rev-parse", "--abbrev-ref", "HEAD"])
    return r.stdout.strip()


def git_has_changes() -> bool:
    r = sh(["git", "status", "--porcelain"])
    return bool(r.stdout.strip())


def git_reset_hard(commit: str):
    sh(["git", "reset", "--hard", commit])


def log(msg: str):
    ts = datetime.datetime.now().strftime("%H:%M:%S")
    line = f"[{ts}] {msg}"
    print(line, flush=True)
    with open(RUN_LOG, "a") as f:
        f.write(line + "\n")


# ---------------------------------------------------------------------------
# CI runner
# ---------------------------------------------------------------------------


def run_ci() -> list[CIResult]:
    """Run all CI checks, return results."""
    results = []
    for name, cmd in CI_COMMANDS:
        t0 = time.time()
        try:
            r = sh(cmd, timeout=300)
            passed = r.returncode == 0
            output = r.stdout + r.stderr
        except subprocess.TimeoutExpired:
            passed = False
            output = f"TIMEOUT after 300s"
        dt = time.time() - t0
        results.append(CIResult(name=name, passed=passed, output=output, duration=dt))
        status = "PASS" if passed else "FAIL"
        log(f"  {name}: {status} ({dt:.1f}s)")
    return results


def ci_all_pass(results: list[CIResult]) -> bool:
    return all(r.passed for r in results)


def ci_summary(results: list[CIResult]) -> str:
    """One-line summary of CI results."""
    parts = []
    for r in results:
        parts.append(f"{r.name}:{'ok' if r.passed else 'FAIL'}")
    return " | ".join(parts)


# ---------------------------------------------------------------------------
# Issue extraction
# ---------------------------------------------------------------------------


def extract_issues(ci_results: list[CIResult]) -> str:
    """Build a summary of all CI issues for Claude to fix."""
    sections = []
    for r in ci_results:
        if not r.passed:
            # Truncate very long output to keep the prompt manageable
            output = r.output
            if len(output) > 4000:
                output = output[:2000] + "\n\n... (truncated) ...\n\n" + output[-2000:]
            sections.append(f"## {r.name} FAILED\n\n```\n{output}\n```")
    if not sections:
        return ""
    return "\n\n".join(sections)


# ---------------------------------------------------------------------------
# Claude Code invocation
# ---------------------------------------------------------------------------


def load_program() -> str:
    """Load the program.md instructions if it exists."""
    if PROGRAM_FILE.exists():
        return PROGRAM_FILE.read_text()
    return ""


def build_prompt(issues: str, iteration: int, history: str) -> str:
    """Build the prompt for Claude Code."""
    program = load_program()

    prompt = textwrap.dedent(f"""\
    You are an autonomous improvement agent for the Orca CLI (a Rust project).
    Your job: fix ONE issue from the CI output below, then stop.

    ## Rules
    - Fix exactly ONE issue per iteration (smallest useful change)
    - Run `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` before finishing
    - All three must pass before you stop
    - Do NOT add unnecessary complexity, comments, or refactoring
    - Do NOT modify test expectations to make tests pass — fix the actual code
    - If the issue is a test failure, read the test to understand what it expects, then fix the code (or the test if the test is genuinely wrong)
    - If you cannot fix the issue after 2 attempts, skip it and report why
    - Commit your fix with a clear message

    ## Current CI Issues (iteration {iteration})

    {issues}

    ## Recent History

    {history}
    """).strip()

    if program:
        prompt = f"{program}\n\n---\n\n{prompt}"

    return prompt


def run_claude(prompt: str) -> tuple[bool, str]:
    """
    Invoke Claude Code in non-interactive mode.
    Returns (success, output_summary).
    """
    log("  Spawning Claude Code...")
    try:
        r = subprocess.run(
            [
                "claude",
                "--print",
                "--dangerously-skip-permissions",
                prompt,
            ],
            capture_output=True,
            text=True,
            timeout=CLAUDE_TIMEOUT,
            cwd=PROJECT_DIR,
        )
        output = r.stdout + r.stderr
        # Truncate for logging
        summary = output[-2000:] if len(output) > 2000 else output
        success = r.returncode == 0
        return success, summary
    except subprocess.TimeoutExpired:
        return False, f"Claude Code timed out after {CLAUDE_TIMEOUT}s"
    except FileNotFoundError:
        return False, "Claude Code CLI ('claude') not found in PATH"


# ---------------------------------------------------------------------------
# Results logging
# ---------------------------------------------------------------------------


def init_results():
    """Create results.tsv with header if it doesn't exist."""
    if not RESULTS_FILE.exists():
        RESULTS_FILE.parent.mkdir(parents=True, exist_ok=True)
        RESULTS_FILE.write_text("iter\tcommit\tci_before\tci_after\tstatus\tdescription\ttimestamp\n")


def log_result(it: Iteration):
    """Append one row to results.tsv."""
    ci_b = ci_summary(it.ci_before)
    ci_a = ci_summary(it.ci_after) if it.ci_after else ""
    desc = it.fix_description.replace("\t", " ").replace("\n", " ")[:120]
    row = f"{it.number}\t{it.commit_after or it.commit_before}\t{ci_b}\t{ci_a}\t{it.status}\t{desc}\t{it.timestamp}\n"
    with open(RESULTS_FILE, "a") as f:
        f.write(row)


def recent_history(n: int = 5) -> str:
    """Return the last n lines from results.tsv for context."""
    if not RESULTS_FILE.exists():
        return "(no history yet)"
    lines = RESULTS_FILE.read_text().strip().split("\n")
    recent = lines[-n:] if len(lines) > n else lines
    return "\n".join(recent)


# ---------------------------------------------------------------------------
# Optional clippy hints (when CI passes)
# ---------------------------------------------------------------------------


def get_optional_clippy_hints() -> str:
    """Run clippy without -D warnings to get optional improvement hints."""
    try:
        r = sh(["cargo", "clippy"], timeout=120)
        if r.returncode != 0 and (r.stdout or r.stderr):
            out = (r.stdout + r.stderr).strip()
            if len(out) > 3000:
                out = out[:1500] + "\n... (truncated) ...\n" + out[-1500:]
            return f"Optional clippy suggestions (non-blocking):\n```\n{out}\n```"
    except (subprocess.TimeoutExpired, Exception):
        pass
    return ""


# ---------------------------------------------------------------------------
# Orca orchestration (spawn worker in worktree, poll, merge, validate)
# ---------------------------------------------------------------------------


def orca_available() -> bool:
    """Return True if orca CLI is on PATH."""
    try:
        subprocess.run(
            ["orca", "--help"],
            capture_output=True,
            timeout=5,
            cwd=PROJECT_DIR,
        )
        return True
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


def orca_spawn_worker(worker_name: str, task: str, base_branch: str) -> bool:
    """Spawn an orca worker with the given task. Returns True if spawn succeeded."""
    log(f"  Spawning orca worker: {worker_name}")
    try:
        r = subprocess.run(
            [
                "orca",
                "spawn",
                task,
                "-b",
                "cc",
                "-d",
                str(PROJECT_DIR),
                "--base-branch",
                base_branch,
                "-n",
                worker_name,
                "--orchestrator",
                "none",
            ],
            capture_output=True,
            text=True,
            timeout=60,
            cwd=PROJECT_DIR,
            env={
                **os.environ,
                "ORCA_ALLOW_SPAWN_WITHOUT_ORCHESTRATOR": "1",
            },
        )
        if r.returncode != 0:
            log(f"  orca spawn failed: {r.stderr[:400] if r.stderr else r.stdout[:400]}")
            return False
        return True
    except FileNotFoundError:
        log("  orca not found in PATH")
        return False
    except subprocess.TimeoutExpired:
        log("  orca spawn timed out")
        return False


def get_worker_status(worker_name: str) -> str:
    """Return worker status string: running, done, dead, destroyed, or empty if not found."""
    try:
        r = subprocess.run(
            ["orca", "status", worker_name],
            capture_output=True,
            text=True,
            timeout=10,
            cwd=PROJECT_DIR,
        )
        if r.returncode != 0:
            return ""
        for line in r.stdout.splitlines():
            if line.strip().lower().startswith("status:"):
                return line.split(":", 1)[1].strip().lower()
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    return ""


def wait_for_worker(worker_name: str, timeout_sec: int = ORCA_WAIT_TIMEOUT) -> str:
    """Poll orca status until worker is done/dead/destroyed. Returns final status."""
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        status = get_worker_status(worker_name)
        if status in ("done", "dead", "destroyed"):
            return status
        time.sleep(ORCA_POLL_INTERVAL)
    return "timeout"


def get_worktree_commit(worker_name: str) -> str | None:
    """Return HEAD commit hash in the worker's worktree, or None if not found."""
    worktree_dir = PROJECT_DIR / ".worktrees" / worker_name
    if not worktree_dir.is_dir():
        return None
    try:
        r = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=5,
            cwd=str(worktree_dir),
        )
        if r.returncode == 0:
            return r.stdout.strip()
        return None
    except (subprocess.TimeoutExpired, Exception):
        return None


def merge_worktree_commit(commit: str) -> bool:
    """Merge the given commit into the current branch. Returns True on success."""
    try:
        r = subprocess.run(
            ["git", "merge", "--no-ff", commit, "-m", f"autoimprove: merge {commit[:7]}"],
            capture_output=True,
            text=True,
            timeout=60,
            cwd=PROJECT_DIR,
        )
        return r.returncode == 0
    except (subprocess.TimeoutExpired, Exception):
        return False


def orca_kill_worker(worker_name: str) -> None:
    """Kill the orca worker and remove its worktree."""
    try:
        subprocess.run(
            ["orca", "kill", worker_name],
            capture_output=True,
            timeout=30,
            cwd=PROJECT_DIR,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired, Exception):
        pass


def cleanup_all_worktrees() -> None:
    """Remove all worktrees under project .worktrees so only the main working tree remains."""
    try:
        r = subprocess.run(
            ["git", "worktree", "list", "--porcelain"],
            capture_output=True,
            text=True,
            timeout=10,
            cwd=PROJECT_DIR,
        )
        if r.returncode != 0:
            return
        worktrees_dir = PROJECT_DIR / ".worktrees"
        if not worktrees_dir.is_dir():
            return
        for line in r.stdout.splitlines():
            if line.startswith("worktree "):
                path = line[9:].strip()
                if path and str(worktrees_dir) in path:
                    try:
                        subprocess.run(
                            ["git", "worktree", "remove", path, "--force"],
                            capture_output=True,
                            timeout=30,
                            cwd=PROJECT_DIR,
                        )
                    except (subprocess.TimeoutExpired, Exception):
                        pass
        subprocess.run(
            ["git", "worktree", "prune"],
            capture_output=True,
            timeout=10,
            cwd=PROJECT_DIR,
        )
        # Remove any leftover .worktrees dirs (e.g. if worktree remove didn't delete)
        for p in worktrees_dir.iterdir():
            if p.is_dir():
                try:
                    shutil.rmtree(p, ignore_errors=True)
                except Exception:
                    pass
    except (subprocess.TimeoutExpired, Exception):
        pass


def orca_worker_logs(worker_name: str, max_lines: int = 500) -> str:
    """Return last part of worker logs for detecting 'nothing to improve' etc."""
    try:
        r = subprocess.run(
            ["orca", "logs", worker_name, "-n", str(max_lines)],
            capture_output=True,
            text=True,
            timeout=30,
            cwd=PROJECT_DIR,
        )
        if r.returncode == 0:
            return (r.stdout or "") + (r.stderr or "")
    except (FileNotFoundError, subprocess.TimeoutExpired, Exception):
        pass
    return ""


def detect_nothing_to_improve(log_output: str) -> bool:
    """Return True if worker reported nothing to improve."""
    lower = log_output.lower()
    return "nothing to improve" in lower or "no improvement" in lower


# ---------------------------------------------------------------------------
# Sprint-team role (coder) injection
# ---------------------------------------------------------------------------


# Guard: orchestrator merges all work into one branch = one PR for human.
SINGLE_PR_BRANCH_GUARD = """\
## Single destination branch
You are one of several workers. The **orchestrator** will merge your commit into its branch after you finish. That branch is the single PR the human will use (one PR with all features). You may push and create a PR from your worktree if you want; the orchestrator still merges your commit into its branch.
"""


def load_sprint_team_role(role_name: str) -> str:
    """Load a sprint-team role template and substitute common vars."""
    path = SPRINT_TEAM_REFERENCES / f"{role_name}.md"
    if not path.exists():
        return ""
    text = path.read_text()
    text = text.replace("{{project_dir}}", str(PROJECT_DIR))
    # {{base_branch}} and {{work_unit_json}} left for build_prompt_orca to fill
    return text


def build_prompt_orca(
    issues: str,
    iteration: int,
    history: str,
    base_branch: str,
    use_sprint_team: bool,
) -> str:
    """Build the task string for orca spawn (single block of instructions)."""
    program = load_program()
    role_prefix = ""
    if use_sprint_team:
        role_prefix = load_sprint_team_role("coder")
        role_prefix = role_prefix.replace("{{base_branch}}", base_branch)
        # Minimal work unit: "fix the following" + issues
        work_unit = f"Fix exactly ONE of the following. Do not refactor unrelated code.\n\n{issues}"
        role_prefix = role_prefix.replace("{{work_unit_json}}", work_unit)
        role_prefix = role_prefix.replace("{{coverage_minimum}}", "0")
        role_prefix = role_prefix.replace("{{coverage_command}}", "cargo test")
        role_prefix = role_prefix.replace("{{coverage_baseline}}", "N/A")
        role_prefix = role_prefix + "\n\n---\n\n"

    prompt = textwrap.dedent(f"""\
    You are an autonomous improvement agent for the Orca CLI (a Rust project).
    Your job: fix ONE issue from the section below, then stop.

    ## Rules
    - Fix exactly ONE issue per iteration (smallest useful change)
    - Run `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` before finishing
    - All three must pass before you stop
    - Do NOT add unnecessary complexity, comments, or refactoring
    - Do NOT modify test expectations just to make tests pass — fix the actual code
    - If you cannot fix the issue after 2 attempts, skip it and report "nothing to improve"
    - Commit your fix with a clear message

    ## Current CI / improvement context (iteration {iteration})

    {issues}

    ## Recent History

    {history}
    """).strip()

    if program:
        prompt = program + "\n\n---\n\n" + prompt
    if role_prefix:
        prompt = role_prefix + prompt
    # Guard: orchestrator merges into one branch = one PR for human
    prompt = SINGLE_PR_BRANCH_GUARD.strip() + "\n\n---\n\n" + prompt
    return prompt


# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------


def run_loop(
    max_iters: int = 0,
    dry_run: bool = False,
    use_orca: bool = False,
    sprint_team: bool = False,
):
    init_results()

    # Dry-run with no max: cap at 3 so we don't spin forever
    effective_max = max_iters
    if dry_run and effective_max == 0:
        effective_max = 3
        log(f"Dry-run with no --max-iters: limiting to {effective_max} iterations")

    # Ensure we're on an autoimprove branch
    branch = git_branch()
    if not branch.startswith("autoimprove/"):
        tag = datetime.datetime.now().strftime("%b%d").lower()
        new_branch = f"autoimprove/{tag}"
        log(f"Creating branch: {new_branch}")
        if not dry_run:
            sh(["git", "checkout", "-b", new_branch])
    base_branch = git_branch()

    if use_orca and not dry_run and not orca_available():
        log("--use-orca set but orca not found in PATH; falling back to direct claude")
        use_orca = False

    iteration = 0
    consecutive_skips = 0

    while True:
        iteration += 1
        if effective_max and iteration > effective_max:
            log(f"Reached max iterations ({effective_max}), stopping.")
            break

        it = Iteration(
            number=iteration,
            timestamp=datetime.datetime.now().isoformat(timespec="seconds"),
            commit_before=git_short_hash(),
        )

        log(f"=== Iteration {iteration} ===")

        # 1. Run CI
        log("Running CI...")
        it.ci_before = run_ci()

        if ci_all_pass(it.ci_before):
            log("All CI checks pass! Looking for improvements...")
            issues = textwrap.dedent("""\
                All CI checks pass. Look for one of these to improve:
                1. A missing test for an untested code path
                2. A clippy warning that could be addressed (run `cargo clippy 2>&1`)
                3. A code simplification opportunity
                4. An edge case that's not handled

                Pick the single highest-value small improvement and implement it.
                If you genuinely cannot find anything useful, report "nothing to improve".
            """)
            hints = get_optional_clippy_hints()
            if hints:
                issues = issues.rstrip() + "\n\n" + hints + "\n"
        else:
            issues = extract_issues(it.ci_before)
            log(f"Issues found:\n{issues[:500]}")

        if dry_run:
            mode = "orca worker" if use_orca else "Claude Code"
            log(f"[DRY RUN] Would invoke {mode} with the issues above.")
            it.status = "skip"
            it.fix_description = "dry run"
            log_result(it)
            continue

        # 2. Invoke worker (orca spawn or direct claude)
        if use_orca:
            worker_name = f"ai-iter-{iteration}"
            task = build_prompt_orca(
                issues, iteration, recent_history(), base_branch, sprint_team
            )
            if not orca_spawn_worker(worker_name, task, base_branch):
                it.status = "skip"
                it.fix_description = "orca spawn failed"
                log_result(it)
                consecutive_skips += 1
                if consecutive_skips >= 3:
                    log("3 consecutive skips — pausing 60s before retry")
                    time.sleep(60)
                continue
            final_status = wait_for_worker(worker_name)
            if final_status == "timeout":
                log("Worker timed out; killing and skipping")
                orca_kill_worker(worker_name)
                it.status = "skip"
                it.fix_description = "worker timeout"
                log_result(it)
                consecutive_skips += 1
                continue
            worktree_commit = get_worktree_commit(worker_name)
            worker_logs = orca_worker_logs(worker_name)
            orca_kill_worker(worker_name)
            if not worktree_commit or worktree_commit == it.commit_before:
                logs = worker_logs
                if detect_nothing_to_improve(logs):
                    it.status = "no_improvement"
                    it.fix_description = "worker reported nothing to improve"
                    log_result(it)
                    # Don't count no_improvement toward consecutive_skips
                else:
                    it.status = "skip"
                    it.fix_description = "no changes from worker"
                    log_result(it)
                    consecutive_skips += 1
                continue
            if not merge_worktree_commit(worktree_commit):
                it.status = "skip"
                it.fix_description = "merge failed"
                log_result(it)
                consecutive_skips += 1
                continue
            it.commit_after = git_short_hash()
            log("Validating fix...")
            it.ci_after = run_ci()
            if ci_all_pass(it.ci_after):
                log(f"FIX ACCEPTED (commit {it.commit_after})")
                it.status = "keep"
                r = sh(["git", "log", "-1", "--pretty=%s"])
                it.fix_description = r.stdout.strip()
                consecutive_skips = 0
            else:
                log("Fix did not pass CI — discarding")
                it.status = "discard"
                it.fix_description = f"fix failed CI: {ci_summary(it.ci_after)}"
                git_reset_hard(it.commit_before)
            log_result(it)
        else:
            # Direct claude path (original behavior)
            prompt = build_prompt(issues, iteration, recent_history())
            claude_ok, claude_output = run_claude(prompt)

            if not claude_ok:
                log(f"Claude Code failed: {claude_output[:300]}")
                it.status = "skip"
                it.fix_description = f"claude failed: {claude_output[:80]}"
                log_result(it)
                consecutive_skips += 1
                if consecutive_skips >= 3:
                    log("3 consecutive skips — pausing 60s before retry")
                    time.sleep(60)
                continue

            if not git_has_changes():
                new_hash = git_short_hash()
                if new_hash == it.commit_before:
                    log("No changes made by Claude. Skipping.")
                    it.status = "skip"
                    it.fix_description = "no changes"
                    log_result(it)
                    consecutive_skips += 1
                    continue

            log("Validating fix...")
            it.ci_after = run_ci()
            it.commit_after = git_short_hash()

            if ci_all_pass(it.ci_after):
                log(f"FIX ACCEPTED (commit {it.commit_after})")
                it.status = "keep"
                r = sh(["git", "log", "-1", "--pretty=%s"])
                it.fix_description = r.stdout.strip()
                consecutive_skips = 0
            else:
                log("Fix did not pass CI — discarding")
                it.status = "discard"
                it.fix_description = f"fix failed CI: {ci_summary(it.ci_after)}"
                git_reset_hard(it.commit_before)

            log_result(it)

        time.sleep(2)

    if use_orca and not dry_run:
        log("Cleaning up worktrees...")
        cleanup_all_worktrees()
    log("Loop finished.")


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description="Orca auto-improvement loop")
    parser.add_argument("--max-iters", type=int, default=0, help="Max iterations (0 = infinite)")
    parser.add_argument("--dry-run", action="store_true", help="Show what would run without executing")
    parser.add_argument(
        "--use-orca",
        action="store_true",
        help="Use orca spawn per iteration (worktree, merge, validate); requires orca on PATH",
    )
    parser.add_argument(
        "--sprint-team",
        action="store_true",
        help="Inject sprint-team coder role into worker task (use with --use-orca)",
    )
    args = parser.parse_args()

    log(
        f"Starting autoimprove loop (max_iters={args.max_iters}, dry_run={args.dry_run}, "
        f"use_orca={args.use_orca}, sprint_team={args.sprint_team})"
    )
    log(f"Project: {PROJECT_DIR}")
    log(f"Branch: {git_branch()}")
    log(f"Commit: {git_short_hash()}")

    try:
        run_loop(
            max_iters=args.max_iters,
            dry_run=args.dry_run,
            use_orca=args.use_orca,
            sprint_team=args.sprint_team,
        )
    except KeyboardInterrupt:
        log("\nInterrupted by user. Cleaning up worktrees...")
        cleanup_all_worktrees()
        log("Goodbye!")
        sys.exit(0)


if __name__ == "__main__":
    main()
