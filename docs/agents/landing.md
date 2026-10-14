# Landing PRs

Many agent threads build and land PRs in parallel. Each thread owns its PR end to end:
it verifies the PR, runs bounded review rounds, resolves its own conflicts, and merges.

## Verify: a swarm breaks each PR

Agents verify behaviour by using it; a growing script suite is not the goal. Every gate
added here slows every PR, so add none.

- A PR that changes Machine behaviour (daemon, Docker, WireGuard, ZFS, moves, deploys)
  sends a swarm of 2-3 agents before it goes ready. Each agent boots its own Boat (the
  `verify-server` skill, or `core/scripts/verify-cluster`) and tries to break the changed
  feature the way a real user would: the happy path, the edges, and what the change might
  break nearby. Give each agent a different angle. Agents drive the CLI and dashboard by
  hand; they write no scenario scripts.
- A swarm agent's report is the commands it ran and the end state it observed. A break it
  finds gets fixed in the PR, then the agent that found it reruns its angle.
- Put each agent's evidence in the PR body.
- A PR that leaves Machine behaviour alone skips Boat. Verify dashboard UI with the
  `verify-dashboard` skill.
- Merges run no scenario suite.

### Durable scenarios

`core/scripts/test-verify-*.py` is frozen: shrink it, never grow it. Delete a scenario
that overlaps another or no longer catches anything. A behaviour worth checking again goes
into the next swarm's angles, not into a script. Releases are Nick's job; agents leave
them alone.

## Review: bounded rounds

| Round | Reviewers         | Scope                  |
| ----- | ----------------- | ---------------------- |
| 1     | Codex and Opus    | Full PR diff           |
| 2     | Codex             | Full PR diff           |
| 3+    | Codex             | Diff of the last fix   |

- Stop at the first round with no BUG-class finding.
- A finding is BUG-class when the PR leaves behaviour worse than main. File a defect that
  main already has as an issue, and keep it out of the PR.
- Only BUG-class findings start another round. Speculative, minor, style, and repeat
  findings get noted or dropped. A round starts for a known finding, never for what a
  reviewer might still find.
- When the same function needs its second fix, rewrite that function's whole contract
  before you patch it again.
- A large push during review restarts at round 1.
- CodeRabbit sits outside the gate. Change code only for a finding you accept on its
  merits.

Why the bound: across 45 PRs and 566 findings, 41% of real bugs came from earlier review
fixes. Speculative findings rose from 24 in round 1 to 76 in round 3 and later. The Codex
panel had a 60% real-bug rate; CodeRabbit had 11%.

## Land

- Open PRs ready, not draft.
- Once review is clean, arm auto-merge: `gh pr merge <n> --auto --merge`. GitHub merges
  when the required "CI ready" check passes and the PR has no conflicts. The branch does
  not need to be up to date with main.
- Keep stacks short and merge each layer as soon as it is green. Ship unfinished work
  dark, behind a flag, in place of a tall stack.

### Conflicts

- The thread that owns the PR resolves its conflicts. Enable rerere
  (`git config rerere.enabled true`) and rebase with `git rebase --update-refs` so every
  layer of a stack moves together.
- Registries are conflict hotspots: `schema.ts`, `cli/mod.rs`, `handlers/mod.rs`,
  `change-log.sources.ts`. Where the code allows, add one file per entry so parallel PRs
  touch different files.

### Main is red

Revert the culprit first, then fix forward in a new PR.
