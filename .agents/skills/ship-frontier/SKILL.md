---
name: ship-frontier
description: "Dispatch supervised implementation of the ticket frontier: Codex implements, Opus supervises, eight cold reads review, PRs land the work."
disable-model-invocation: true
---

# Ship The Frontier

Work the **frontier** — every open ticket whose blockers are all closed — as
a **dispatcher**: claim tickets, spawn one Opus **supervisor** per ticket,
and report their progress. All code moves through supervisors; Codex writes
it. Routing lives in `model-selection`, Codex mechanics in
`controlling-codex`, and the repo's gates and review-lane mapping in its
agent instructions (AGENTS.md).

## Dispatch

1. Build the frontier from the tracker's blocked-by edges. A ticket someone
   else holds — an assignee, or its branch checked out in a foreign
   worktree — stays theirs. Done when: every open ticket is blocked,
   foreign, or claimed by you.
2. **Claim** a ticket (assign yourself), then spawn its supervisor: an Opus
   subagent given the ticket number, the repo path, and this skill. Run two
   or three supervisors at a time — the machine's compile and e2e capacity
   is the ceiling.
3. Track each supervisor to its completion criterion; resume one that
   stalls. Read the PR and issue state before reporting a ticket done.
   Done when: the frontier is empty and every spawned ticket is landed or
   handed back with its blocker named.

## Supervise — one ticket, claim to close

Done when: the PR is merged, the issue carries the merge commit and
verification gist and is closed, and the worktree is removed.

1. **Orient** — read the repo's agent instructions, the ticket, and the
   spec it points at; verify the paths and seams yourself before prompting
   Codex. Work in a fresh worktree on a new branch from origin/main.
2. **Implement** — Codex writes the code per `controlling-codex`; steer
   between turns. `git diff main` is ground truth over narration.
3. **Gates** — run every gate the repo declares. Done when: all gates are
   green on the branch as it will merge.
4. **Review** — four lanes: standards, spec, thermo-nuclear, ponytail,
   mapped to skills by the repo. Each lane runs twice — once as a Claude
   subagent, once as a Codex run — so every lane gets a **second opinion**:
   eight parallel **cold reads**, each given only the diff range, the
   lane's skill, and "report back your findings". A seconded finding is
   real — fix it; judge unseconded findings on merit. A fix reruns the
   gates. Done when: the PR body carries every finding's disposition.
5. **Land** — push and open a PR titled after the ticket. If origin/main
   moved, merge it in and rerun the gates first; PRs land one at a time and
   the later one re-verifies. Merge, comment the merge commit and
   verification on the issue, close it, remove the worktree.
