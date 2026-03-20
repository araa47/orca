# autoimprove [WIP]

Autonomous improvement loop for the Orca CLI. Inspired by [karpathy/autoresearch](https://github.com/karpathy/autoresearch). One run produces **one branch** with all accepted changes; create **one PR** from it when done.

The **main process is the orchestrator**: it runs CI, finds issues (or improvement ideas when CI passes), delegates to a worker to fix one thing, then validates and keeps or discards. Workers can run either in-process (direct Claude) or as orca workers in separate worktrees.

## Quick start

```bash
# Run the loop (Ctrl+C to stop) — direct Claude in-process
uv run autoimprove/loop.py

# Use orca: one worker per iteration (worktree, merge, validate)
uv run autoimprove/loop.py --use-orca

# Orca + sprint-team coder role in the task
uv run autoimprove/loop.py --use-orca --sprint-team

# Run 5 iterations then stop
uv run autoimprove/loop.py --max-iters 5

# Dry run — show what would happen (limited to 3 iters if no --max-iters)
uv run autoimprove/loop.py --dry-run
```

**Requirements**: [uv](https://docs.astral.sh/uv/), [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) (`claude` in PATH). For `--use-orca`: orca on PATH and daemon (auto-started on first spawn).

## How it works

- The script always runs on an **autoimprove branch**. If the current branch is not `autoimprove/*`, it creates `autoimprove/<date>` (e.g. `autoimprove/mar19`) and checks it out.
- **Without `--use-orca`**: each iteration runs `claude` in the project dir; changes are committed there and CI is re-run; on failure the last commit is reset.
- **With `--use-orca`**: each iteration spawns one orca worker (e.g. `ai-iter-1`) with `--base-branch` set to the current autoimprove branch. The worker runs in a worktree (`.worktrees/ai-iter-N`). When the worker is done, the loop merges the worktree’s HEAD into the main branch, runs CI, keeps or discards the merge, then kills the worker and removes the worktree.

```
LOOP:
  1. Run CI (fmt, clippy, test)
  2. If failures → extract error output; if all pass → build “find an improvement” prompt (+ optional clippy hints)
  3. Invoke worker (direct claude OR orca spawn with task)
  4. Worker fixes ONE thing and commits (in worktree if orca)
  5. If orca: merge worktree commit into current branch
  6. Re-run CI to validate
  7. Keep if CI passes, discard (git reset) if not
  8. Log to results.tsv (status: keep | discard | skip | no_improvement)
  9. GOTO 1
```

## Single PR result

All iterations contribute to the **same** branch (e.g. `autoimprove/mar19`). Workers may push and create PRs from their worktrees if they want; the **orchestrator** merges each accepted change into its branch. That branch is the single clear destination: one PR with all features for the human. After the loop finishes (or you stop it), push and open that one PR:

```bash
git push -u origin autoimprove/mar19
gh pr create --base main --fill
```

When the loop exits (normally or Ctrl+C), all worktrees under `.worktrees/` are removed so only the main working tree remains.

## Sprint-team integration

With `--sprint-team`, the task passed to the worker is prefixed with the **coder** role from `.agents/skills/sprint-team/references/coder.md` (with `{{project_dir}}`, `{{base_branch}}`, and the current issues substituted). Use this when you want the worker to follow the sprint coder workflow (TDD, coverage, etc.).

## Files

| File | Purpose |
|------|---------|
| `loop.py` | The loop script (uv inline deps, single file) |
| `program.md` | Instructions and constraints for the worker |
| `results.tsv` | Experiment log (auto-created, not committed) |
| `run.log` | Detailed runtime log |

## Customization

Edit `program.md` to change what the worker focuses on. The loop itself (`loop.py`) is just plumbing.
