---
name: soak
description: Run a release soak, where a supervisor thread fans out worker threads that use Ployz like real users and file findings. Use when planning, briefing or supervising a soak, or when working as a soak worker.
---

# Soak a release

A soak lives in a run store, `~/.claude/orchestrate/<run>/`. Its `BRIEF.md` holds the facts for this release only: the version and install line, the areas and their workers, the orgs, lab quirks and dashboard routes. Product fixes change those every release, so they stay in the brief. This skill holds the process that outlasts a release. Read [verify-server](../verify-server/SKILL.md) for real Machines and [verify-dashboard](../verify-dashboard/SKILL.md) for a local Cloud.

## Before workers start

The supervisor:

1. Creates every org a worker uses (`soak-<area>-1`) and mints each worker's token. The brief names each worker's org.
2. Approves or denies any pending approval in those orgs, so a worker's keyboard pass can't answer one by accident.
3. Gives each worker a scratch dir in the store and its own `PLOYZ_CONFIG`. The active org lives on the server and is shared by every session of one account, so each worker checks the account menu before a write in the browser.
4. Names which worker's run hosts a local Cloud. That run goes down last.
5. Plans timed steps, such as cleanup and the end of the soak, as its own sends. A worker's in-session cron stays silent while the worker's thread is idle.

## Reporting

Workers keep findings in files and send the supervisor pointers, so the supervisor's context stays small and it triages with subagents.

- Append each finding to the store's `FINDINGS.md` and its evidence under `evidence/<area>/`. Ids run per area (`<area>-<n>`), so only you take ids from your own sequence. Read your area's last id and append in one step under the lock, `flock FINDINGS.md.lock`.
- Each row names the Machine or container checked, the exact command or click, and what came back. Flag any lab action that came first: a Boat stop, resume or `power on`.
- Grep `docs/user/` before calling a behaviour unexpected. Documented behaviour is a docs or design question.
- Probe whether a destroy waits for approval. The brief may be wrong about it.
- Send the supervisor one line, the finding ids or a file path, with `t3_thread_send` mode `auto`. `auto` steers a running turn and starts an idle one. On `parent_not_active`, append the line to `outbox/<area>.md`. The supervisor reads `outbox/` at the start of every turn and moves each file it has handled to `outbox/read/`.

## Secrets

- Redact before output reaches a file or the transcript: filter, then `tee`. Cover tokens, JWTs, enrollment codes, passwords, session cookies and invite links (`/invite/…`).
- Read one field at a time with `jq` from files that hold tokens: `seed.json`, `.verify/run/info`, up logs, and any `--json` output that mints a token.
- Clean a leaked secret by matching its pattern. Typing the secret into a redaction command logs it again.

## Boat

A soak outlasts the one-hour auto-stop. Follow verify-server's [Long runs and stopped sandboxes](../verify-server/SKILL.md#long-runs-and-stopped-sandboxes): extend each sandbox up front, and replace a stopped run instead of resuming it.

## Cleanup and retro

The supervisor:

1. Runs every check against an org before `org rm`. Deleting an org revokes its maker's token, so nothing can read the org afterwards.
2. Revokes the workers' agent tokens. An agent token can't revoke itself, so any left over go to Nick to remove on the Tokens page.
3. Downs every run, then confirms with `verify-cluster ls` and `boat list`.
4. Asks each worker for a retro in `retro/<area>.md`, in four sections: friction, brief gaps, skill and doc gaps, and findings never filed. Triage `retro/` with subagents, the same way as `FINDINGS.md`.
