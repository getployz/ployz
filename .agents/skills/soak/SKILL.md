---
name: soak
description: Run a release soak, where a supervisor thread fans out worker threads that use Ployz like real users and file findings. Use when planning, briefing or supervising a soak, or when working as a soak worker.
---

# Soak a release

A soak lives in a run store, `~/.claude/orchestrate/<run>/`. Its `BRIEF.md` holds the facts for this release only: the version and install line, the areas and their workers, the orgs, lab quirks and dashboard routes. Product fixes change those every release, so they stay in the brief. This skill holds the process that outlasts a release. Read [verify-server](../verify-server/SKILL.md) for real Machines and [verify-dashboard](../verify-dashboard/SKILL.md) for a local Cloud.

## Before workers start

The supervisor:

1. Creates every org a worker uses (`soak-<area>-1`) and mints each worker's token through `keep` (see [Secrets](#secrets)). The brief names each worker's org.
2. Approves or denies any pending approval in those orgs, so a worker's keyboard pass can't answer one by accident.
3. Gives each worker a scratch dir in the store and a guarded CLI for every `ployz` binary it drives (release, N-1, checkout): `.agents/skills/soak/scripts/mkguard <store>/scratch/<area>/<who> <real-ployz>` prints the wrapper's path. The wrapper pins `PLOYZ_CONFIG` to that dir and exits 97 on ployz.dev. Every brief hands workers wrapper paths, never a bare binary. Isolation written only as prose once let a worker deploy into Nick's production org. `verify-cluster cli` isolates its own config. The active org lives on the server and is shared by every session of one account, so each worker checks the account menu before a write in the browser.
4. Names which worker's run hosts a local Cloud. That run goes down last.
5. Plans timed steps, such as cleanup and the end of the soak, as its own sends. A worker's in-session cron stays silent while the worker's thread is idle.
6. Assigns a gap sweep when the release touches a shared write or read path. One worker lists every caller from the code first: dashboard, CLI, MCP and agent tools, presets and templates, and every backend. Then it drives each caller. Area rows alone missed a refused dashboard Create → Database through two full passes.
7. Turns the previous soak's flag-on findings into live rows when a PR removes a feature flag. Deleting a flag turns its behaviour on for every tenant.

Standalone Clusters are a test-only mode, by Nick's ruling. Every lab enrolls through a Cloud, and a Standalone finding is never a blocker and never goes to Nick.

## Kinds of check

Each kind proves one thing. A PASS of one kind proves nothing about another, so a verdict names the kinds it ran.

- **Focus rows** drive the release's own changes. They prove the changed paths, not the paths around them.
- **N-1 compat** runs the N-1 CLI and N-1 Servers under the new Cloud. It proves old clients keep working, not that a fresh install does. A lab N-1 Server is not a genuine one; see verify-server's [Cloud pairing](../verify-server/SKILL.md#cloud-pairing).
- **Rolling Corrosion and daemon upgrade under the ack probe** is a must-row every release. It proves acknowledged writes survive the upgrade and measures the write gap. Report lost acks and the longest gap next to the previous release's numbers. `verify-cluster update` restarts every daemon at each step, so it doesn't model a real one-at-a-time roll.
- **Self-host Cloud upgrade, forced rollback and roll forward** proves the documented snippet and that each Cloud version reads the other's data. It says nothing about Servers.
- **Phase A and Phase B release checks** prove the release artifacts; see [Release phases](#release-phases).
- **Fault injection** proves behaviour under the faults driven, in the state window each check acted in. Follow verify-server's [Faults and the ack probe](../verify-server/SKILL.md#faults-and-the-ack-probe).
- **Healthy-mesh watch** leaves a mesh unfaulted and must stay quiet: no warnings, lowered routes or new journal causes. It proves there are no false positives, not that detection works.
- **Gap sweep** (step 6 above) proves every caller of a shared path works, not depth on any one.
- **Promise audit** is a rotating row each release that drives one guarantee in `docs/user/` under faults, for example Volumes while their holder Server is Down. Diff-focused rows find only regressions; this finds a promise that was never kept.

## Release phases

- **Phase A, on the draft assets.** Serve an https mirror of the exact draft files with a real certificate: Caddy with Let's Encrypt on a Boat host, named by its IPv6 address on sslip.io (`<address-with-dashes>.sslip.io`). Lay them out as `getployz/ployz/releases/download/v<version>/…` with `install.sh` at the root, and verify with the real release key, never a key override. Point Machines at it with `PLOYZ_GITHUB_URL` and `PLOYZ_FALLBACK_URL` in a `ployz.service` drop-in. Add `PLOYZ_CHANNEL_URL` and serve `v0/stable`, since Cloud reads channels only from ployz.sh. The mirror has only an IPv6 address, so run client-side `server add` downloads from an IPv6 host.
- **Phase B, on the published release.** Use no overrides at all. Check that the tarball hashes equal Phase A's.
- **What each proves.** The mirror proves the bytes and the upgrade flow. Only the published artifact proves the install lines, channels and Homebrew.

The supervisor acknowledges a post-tag request as soon as it arrives.

## Real-world hosts

Every soak and pre-release check gives one worker the [real-world hosts axis](real-world-hosts.md): messy hostnames, the newest and an older distribution, Docker or a firewall already there, IPv6-only, small and full disks, low RAM, a flaky network and clock skew, each driven through the install line, cloud-init and `ployz server add` to a first deploy.

## Reporting

Workers keep findings in files and send the supervisor pointers, so the supervisor's context stays small and it triages with subagents.

- Append each finding to the store's `FINDINGS.md` and its evidence under `evidence/<area>/`. Ids run per area (`<area>-<n>`), so only you take ids from your own sequence. Read your area's last id and append in one step under the lock, `flock FINDINGS.md.lock`.
- Each row names the Machine or container checked, the exact command or click, and what came back. Flag any lab action that came first: a Boat stop, resume or `power on`.
- Grep `docs/user/` before calling a behaviour unexpected. Documented behaviour is a docs or design question.
- Search open issues before filing, with `gh issue list -R getployz/ployz --state open --search "<keywords>"`. When one matches, write the issue number in your `FINDINGS.md` row and add your evidence there instead of filing it again.
- Probe whether a destroy waits for approval. The brief may be wrong about it.
- Fill a row's `regression?` only after running the same row on N-1 binaries. Data loss, and a deliberate compat break for an old client, go to Nick whatever their age. "Not a regression" never means safe to ship.
- Send the supervisor one line, the finding ids or a file path, with `t3_thread_send` mode `auto`. `auto` steers a running turn and starts an idle one. On `parent_not_active`, append the line to `outbox/<area>.md`. At the start of every turn the supervisor moves each file in `outbox/` to `outbox/read/<area>-<hhmm>.md` first, then reads the moved copy, so a line appended meanwhile lands in a new file.

## Secrets

- Redact before output reaches a file or the transcript: `cmd 2>&1 | .agents/skills/soak/scripts/redact | tee <log>`. Pipe every grep over scratch logs through it too; an unfiltered grep printed a lab session cookie. It masks `pflt_`, `plyz_`, `pmet_` and `ployzgrant1:` tokens, JWTs, Cookie and Set-Cookie values, `session_token`, Authorization values, invite links (`/invite/…`), PEM blocks and JSON fields named like a token, cookie, password or secret. Enrollment codes and passwords printed bare still need your own pattern.
- Print a Generated Value's length and fingerprint, never the value itself.
- `org approval approve` needs a terminal on stdout. Run it under `script` and pipe `script`'s own output through the filter; see verify-server's [Drive the feature](../verify-server/SKILL.md#drive-the-feature).
- Mint through `.agents/skills/soak/scripts/keep <file> -- <command>`: anything that prints a token, such as `--json server token create`. It writes stdout to a mode-600 file and prints only the path and each field's length and token prefix.
- Read one field at a time with `jq` from files that hold tokens: `seed.json`, `.verify/run/cookie`, up logs, and every file `keep` wrote. `up.sh` and `.verify/run/info` print no secrets; their `cookies set` lines read the cookie from its file.
- Clean a leaked secret by matching its pattern. Typing the secret into a redaction command logs it again.

## Boat

A soak outlasts the one-hour auto-stop. Follow verify-server's [Long runs and stopped sandboxes](../verify-server/SKILL.md#long-runs-and-stopped-sandboxes): extend each sandbox up front, and replace a stopped run instead of resuming it.

## Cleanup and retro

The supervisor:

1. Runs every check against an org before `org rm`. Deleting an org revokes its maker's token, so nothing can read the org afterwards.
2. Revokes the workers' agent tokens. An agent token can't revoke itself, so any left over go to Nick to remove on the Tokens page.
3. Downs every run, then confirms with `verify-cluster ls` and `boat list`.
4. Asks each worker for a retro in `retro/<area>.md`, in four sections: friction, brief gaps, skill and doc gaps, and findings never filed. Triage `retro/` with subagents, the same way as `FINDINGS.md`.
5. Writes the verdict: PASS or BLOCKER for the version at its exact commit, the kinds of check run, and each finding's severity, issue and `regression?`. Two more lines close it: **Not covered (lab limits)**, what the lab couldn't test; and **Known high, pre-existing**, the high-severity findings that predate this release. Standalone stays out of both.
6. Edits every skill and doc gap that retros from two or more runs raise into soak, verify-server or verify-dashboard before the run store closes.
