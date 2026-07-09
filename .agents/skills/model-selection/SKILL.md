---
name: model-selection
description: Mandatory model routing policy only inside Claude Code when choosing Claude models for Agent/Workflow subagents, reviews, implementation, investigation, data analysis, migrations, UI/UX, copy, API design, or delegating to Codex via the Codex CLI. Use this to pick among Claude models, escalate when quality misses the bar, and never let cost beat intelligence or taste.
---

# Picking Models

Rankings are higher-is-better, including cost: a HIGH cost score means CHEAP, a LOW cost score means EXPENSIVE. `codex` (cost 9) is by far the cheapest; `fable-5` (cost 2) is super expensive — never describe it as cheap, and never spend it where `codex` suffices. Cost reflects what the user actually pays, not list price. Intelligence means how hard a problem can be handed to the model unsupervised. Taste covers UI/UX, code quality, API design, and copy.

| model | cost (9 = cheapest) | intelligence | taste |
| --- | ---: | ---: | ---: |
| codex | 9 | 8 | 5 |
| sonnet-5 | 5 | 5 | 7 |
| opus-4.8 | 4 | 7 | 8 |
| fable-5 | 2 | 9 | 9 |

## Apply

- Treat these as defaults, not limits. Override them when output quality demands it. If a cheaper model's output does not meet the bar, rerun or redo the work with a smarter model without asking.
- Use cost as a tie-breaker only. For anything that ships, resolve conflicts as intelligence > taste > cost.
- Default ALL implementation to `codex` — including bug fixes, correctness fixes, and refactors — whenever the task has a written spec: a plan doc, an issue, or a review finding with file:line references. A reviewed finding IS a clear spec. Do not route implementation to Claude models because the bug is "subtle"; subtlety was the reviewer's job, the fix is execution.
- Default investigation and research (crate/library evaluation, codebase scouting, docs legwork) to `codex` as well. Escalate to a Claude model only when the deliverable is a judgment that ships (an architecture decision, a review verdict) — not for gathering.
- Require taste >= 7 for user-facing work: UI, copy, and API design.
- Claude models (`fable-5`, `opus-4.8`) implement only when: (a) `codex`'s output failed a correctness review, or (b) the task has no written spec and requires taste >= 7 (UI, copy, API design).
- When (a) fires, the corrector is `opus-4.8`, not `fable-5`: an opus fix followed by a review pass is as reliable as a fable fix, and fable-5 has its own separate usage limit that draws down much faster than opus. Escalate the corrector to `fable-5` only after an opus fix itself fails review.
- After any `codex` implementation batch, run a Claude review pass (`fable-5` or `opus-4.8`) for correctness before reporting done. Codex writes, Claude verifies.
- Use `fable-5` or `opus-4.8` for reviews of plans and implementations. Add `codex` only as an extra independent perspective.
- Never use Haiku.

## Mechanics

- `codex` is reachable only through the Codex CLI: `codex exec` for implementation and investigation, `codex review` for code review. It adopts user-level configuration from `~/.codex/config.toml` (defaults to gpt-5.5).
- Never delegate through the `openai/codex-plugin-cc` plugin, `/codex:*` slash commands, or the `codex:codex-rescue` subagent — the rescue path is a fire-and-forget forwarder that cannot poll, verify, or commit. Run the CLI directly with Bash.
- Quick one-shot work: `codex exec --cd <dir> "<prompt>"`. Investigation and anything read-only: `codex exec -s read-only "<prompt>"`. Substantive runs use the supervisor pattern below, with mechanics from the `controlling-codex` skill.
- Prompts must be self-contained: inline the spec (ticket body, decision text, acceptance criteria) and name the relevant paths. Do not assume the Codex sandbox can reach GitHub or the network.
- For parallel batches: one durable git worktree per task on its own branch, one supervised Codex run per worktree (see `Controlling Codex`). The coordinator finishes with one Claude review over the combined diff.
- Run Claude models (`sonnet-5`, `opus-4.8`, `fable-5`) through the Agent/Workflow `model` parameter.

## Controlling Codex

All run mechanics — launch, monitoring, steering, recovery — live in the
`controlling-codex` skill (`.agents/skills/controlling-codex/SKILL.md`). Read
it before any supervised run; do not improvise from memory.

### The supervisor pattern

Never fire a substantive Codex run unsupervised. Spawn one `opus-4.8` subagent per run (Agent tool, `model` parameter) as its supervisor. The supervisor:

- owns the Codex run per the `controlling-codex` skill and starts it with the self-contained spec,
- runs the repo `/implement` flow through Codex: TDD at existing seams where possible, checks run regularly, the full touched-crate suite once at the end,
- watches the event stream and steers the moment the run drifts — wrong files, scope creep, misread requirement — or interrupts when it is unrecoverable,
- verifies on turn completion (fmt, clippy, tests for touched crates), steers follow-up turns to fix failures,
- ends with the repo `/code-review` skill over the branch diff (fixed point: the branch base; spec: the ticket), and auto-resolves real findings by steering follow-up Codex turns until the review is clean — judgement-call smells may be dismissed with a stated reason,
- commits the result on the task branch and reports what was built, every steer issued, and the review outcome.

One supervisor per run; parallel runs get parallel supervisors in disjoint worktrees. The coordinator's job stays the same: assemble specs, spawn supervisors, and run the final Claude review over the combined diff.
