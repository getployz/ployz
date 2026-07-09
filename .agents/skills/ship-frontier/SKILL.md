---
name: ship-frontier
description: "Work the ticket frontier as a dispatcher: one Opus supervisor per unblocked ticket, Codex does the implementation, gates plus four review lanes run by two engines each (eight cold reads) decide done, PRs land the work on main. Use when asked to work the frontier, ship unblocked tickets, or run the supervised implementation workflow (pairs with /wayfinder)."
---

# Ship The Frontier

The session that invokes this skill is the **dispatcher**. It never implements.
It selects tickets, spawns one **supervisor** per ticket, watches them, and
reports. Each supervisor is an Opus subagent that owns exactly one ticket from
claim to closed issue. The supervisor never types the implementation either:
**Codex implements** under the supervisor's direction. The supervisor's job is
orientation, gates, reviews, and landing.

Roles and routing come from `model-selection`; Codex run mechanics come from
`controlling-codex`. Read both before spawning anything; do not improvise
run mechanics from memory.

## Dispatcher

1. Build the frontier: open tickets whose `Blocked by:` issues are all closed
   (`gh issue view` the bodies; a ticket with no blockers is frontier).
   Skip tickets another session owns — an assignee that is not you, or a
   ticket branch checked out in a foreign worktree (`git branch -a` shows
   `+`), means hands off.
2. Claim before spawning: assign yourself on the issue.
3. Spawn one Opus supervisor per claimed ticket (background). Give each the
   ticket number, the repo path, and this skill; nothing else — the
   supervisor does its own orientation.
4. Concurrency is bounded by the machine, not the model: every supervisor
   compiles the workspace and may need the DinD harness. Two to three
   supervisors is the practical ceiling; beyond that they starve each other.
5. Watch, don't hover. Resume a supervisor that stalls (a finished fix turn
   with no follow-through, a final message that is a raw tool call). Never
   treat a completion notification as success — read the report, check the
   PR and issue state.
6. When the frontier empties or new tickets unblock, rebuild it and continue.

## Supervisor: one ticket, claim to close

### Orient

- Read AGENTS.md, the ticket body and comments, and the spec section it
  points at. Verify the orientation yourself — file paths, existing seams,
  prior art — before prompting Codex. A supervisor that forwards the ticket
  text unverified produces drift.
- Work in a fresh worktree on a new branch from `origin/main`
  (`git worktree add .claude/worktrees/<ticket> -b pr/<ticket> origin/main`).
  Never check out a branch another worktree holds.

### Implement (Codex)

- Drive the implementation with Codex per `controlling-codex`: first-turn
  prompt with process overrides, prohibitions, a verified orientation map,
  tripwires, and the spec inline; steer between turns with resume. Ground
  truth for drift is `git diff main`, not the narration.
- TDD at pre-agreed seams where the ticket shape allows (`/tdd`).

### Gates — all must pass before review

- `cargo fmt --all` leaves no diff.
- `cargo clippy --workspace --all-targets` reports zero warnings.
- `cargo test --workspace` is green. Grep the output for
  `test result: FAILED` and `error[` — never trust an exit code that passed
  through a pipe (zsh: `${pipestatus[1]}`).
- If `ployz-sdk-types` or the TS surface changed: regenerate from
  `packages/ployz-sdk` (`pnpm generate:types && pnpm generate:fixture`) and
  commit the drift.
- If product behavior changed: the full gated DinD suite
  (`scripts/dind-e2e.sh`), all scenarios. One cluster fits the host —
  serialize with the lock:
  `until mkdir /tmp/ployz-dind-e2e.lock 2>/dev/null; do sleep 30; done`,
  and always `rmdir` it afterwards, including on failure. First run after a
  merge must rebuild product binaries (no `PLOYZ_DIND_SKIP_BUILD`). On
  failure, read the printed evidence dir and container journals before
  retrying; a harness that was green on main fails because of your change.

### Review — four lanes, two engines each, after gates

Four review lanes, each run **twice**: once as a fresh Claude subagent and
once as a Codex run (read-only, per `controlling-codex`). Two independent
opinions per lane — eight reviews total, all launched in parallel.

Every one of the eight is a **cold read**: a fresh context given a
deliberately minimal prompt — the diff range, the lane's skill file, and
"report back your findings". No steering, no pre-seeded suspicions, no
design justifications. A warmed-up reviewer confirms instead of reviewing.

1. **Standards** — the `/code-review` standards axis: does the diff follow
   the repo's documented rules?
2. **Spec** — the `/code-review` spec axis: does the diff do what the
   ticket asked?
3. **Thermo-nuclear** — point the reviewer at
   `.agents/skills/thermo-nuclear-code-quality-review/SKILL.md`.
4. **Ponytail** — point the reviewer at the `ponytail-review` skill; it
   hunts over-engineering only.

Merge the findings: dedup across engines within a lane. A finding both
engines raise is near-certainly real — fix it. A single-engine finding gets
judged on its merits, not dismissed for being unseconded. Resolve with
judgment, not obedience: fix what is real, record the disposition of what
is not (the PR body carries both). Any fix that changes code reruns the
gates. Thermo pushes toward restructuring and ponytail toward deletion —
when they collide, the smaller diff that keeps the invariants wins.

### Land — PR, merge, close

- Push the branch; open a PR titled after the ticket (`... (#N)`); body =
  what landed, gate evidence, review dispositions, and the session link.
- Before merging: if `origin/main` moved, merge it into the branch and
  re-resolve — union imports, keep both sides' functions, and add the arms
  each side's exhaustive matches need for the other's new enum variants —
  then rerun gates. The no-wildcard-arm rule is what catches cross-branch
  drift at compile time; keep it.
- `gh pr merge --merge`. One PR lands at a time; whoever is second
  re-merges and re-verifies.
- Comment on the ticket (merge commit, what was verified), close it, remove
  the worktree.

## Commit rules

- No self-attribution in commit messages or PR bodies.
- End every commit message with the running session's `Claude-Session:`
  trailer line; end PR bodies with the session URL.
- Do not `docker rm` containers you did not create.
