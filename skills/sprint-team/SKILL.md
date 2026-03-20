---
name: sprint-team
description: >-
  Sprint-based improvement team with specialized agent roles. Use when running
  the orca self-improvement loop or any multi-agent sprint workflow. Provides
  reusable role definitions for researcher, architect, reviewers, coder,
  validator, integrator, and retrospector agents.
---
# Sprint Team — Agent Role Definitions

This skill defines the team roles for a sprint-based improvement loop. Each role
is a standalone markdown file in `references/` that can be loaded as a prompt
template by the orchestrator or by agents directly.

## Roles

| Role | File | Phase | Purpose |
|------|------|-------|---------|
| Researcher | [references/researcher.md](references/researcher.md) | Research | Scans codebase for bugs, tech debt, coverage gaps |
| Architect | [references/architect.md](references/architect.md) | Plan | Produces implementation plan with work units |
| Security Reviewer | [references/security-reviewer.md](references/security-reviewer.md) | Review Gate | Reviews plan for security implications |
| Quality Reviewer | [references/quality-reviewer.md](references/quality-reviewer.md) | Review Gate | Reviews plan for test coverage and correctness |
| Scope Reviewer | [references/scope-reviewer.md](references/scope-reviewer.md) | Review Gate | Reviews plan for feasibility and blast radius |
| Coder | [references/coder.md](references/coder.md) | Execute | Implements work units with TDD |
| Validator | [references/validator.md](references/validator.md) | Validate | Independently verifies coder's work (cross-model) |
| Integrator | [references/integrator.md](references/integrator.md) | Integrate | Merges sub-PRs, runs full suite, fixes breakage |
| Retrospector | [references/retrospector.md](references/retrospector.md) | Retrospective | Captures learnings for future runs |

## Sprint Pipeline

```
Research -> Plan -> Review Gate -> Decompose -> Execute -> Validate -> Integrate -> PR -> Retrospective
```

## Template Variables

Role files use `{{variable}}` placeholders. The orchestrator substitutes these
at spawn time:

| Variable | Description |
|----------|-------------|
| `{{project_dir}}` | Absolute path to the project |
| `{{base_branch}}` | The improvement branch name |
| `{{src_files}}` | Newline-separated list of source files to review |
| `{{knowledge_entries}}` | Prior knowledge base entries (may be empty) |
| `{{coverage_minimum}}` | Coverage floor percentage (e.g. 90) |
| `{{coverage_command}}` | Full enforcement command |
| `{{coverage_baseline}}` | Coverage percentage at sprint start |
| `{{work_unit_json}}` | JSON blob describing one work unit |
| `{{coder_backend}}` | Backend the coder used (for cross-model validation) |
| `{{focus_areas}}` | Reviewer-specific focus areas |

## Usage

Any agent can read a role file and adopt that persona:

```bash
# As an orchestrator spawning a researcher
orca spawn "$(cat skills/sprint-team/references/researcher.md)" \
  -b cc -d ~/proj --base-branch improve/sprint-xxx --orchestrator cc
```

The orchestrator loads these files and substitutes the template variables before
spawning workers.
