# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.0.1] - 2025-03-19

### Added

- Initial stable Rust release.
- CLI: spawn, list, logs, steer, kill, gc, pane, report, daemon, hooks.
- Isolated workers in git worktrees; tmux-based monitoring and notifications.
- Support for OpenClaw, Claude Code, Codex, and Cursor as orchestrator backends.
- Claude Code, Codex, and Cursor as worker backends.
- Pre-commit/prek hooks (fmt, clippy, yaml, codespell); CI split into job-prek and job-test.

[Unreleased]: https://github.com/araa47/orca/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/araa47/orca/releases/tag/v0.0.1
