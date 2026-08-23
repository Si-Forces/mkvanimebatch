# AGENTS.md

## Workflow rules

### Code review before every commit

Always run the code review skill (Standards + Spec axes, parallel sub-agents) on the full diff before committing. Fix or explicitly accept findings first, then commit.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues (`sifahaz/mkvanimebatch`). See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical triage roles mapped to GitHub issue labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout (`CONTEXT.md` + `docs/adr/` at repo root). See `docs/agents/domain.md`.
