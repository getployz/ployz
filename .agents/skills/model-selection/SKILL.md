---
name: model-selection
description: Mandatory model routing policy only inside Claude Code when choosing Claude models for Agent/Workflow subagents, reviews, implementation, investigation, data analysis, migrations, UI/UX, copy, API design, or delegating to Codex via openai/codex-plugin-cc. Use this to pick among Claude models, escalate when quality misses the bar, and never let cost beat intelligence or taste.
---

# Picking Models

Rankings are higher-is-better. Cost reflects what the user actually pays, not list price. Intelligence means how hard a problem can be handed to the model unsupervised. Taste covers UI/UX, code quality, API design, and copy.

| model | cost | intelligence | taste |
| --- | ---: | ---: | ---: |
| gpt-5.5 | 9 | 8 | 5 |
| sonnet-5 | 5 | 5 | 7 |
| opus-4.8 | 4 | 7 | 8 |
| fable-5 | 2 | 9 | 9 |

## Apply

- Treat these as defaults, not limits. Override them when output quality demands it. If a cheaper model's output does not meet the bar, rerun or redo the work with a smarter model without asking.
- Use cost as a tie-breaker only. For anything that ships, resolve conflicts as intelligence > taste > cost.
- Default ALL implementation to `gpt-5.5` — including bug fixes, correctness fixes, and refactors — whenever the task has a written spec: a plan doc, an issue, or a review finding with file:line references. A reviewed finding IS a clear spec. Do not route implementation to Claude models because the bug is "subtle"; subtlety was the reviewer's job, the fix is execution.
- Require taste >= 7 for user-facing work: UI, copy, and API design.
- Claude models (`fable-5`, `opus-4.8`) implement only when: (a) `gpt-5.5`'s output failed a correctness review, or (b) the task has no written spec and requires taste >= 7 (UI, copy, API design).
- After any `gpt-5.5` implementation batch, run a Claude review pass (`fable-5` or `opus-4.8`) for correctness before reporting done. Codex writes, Claude verifies.
- Use `fable-5` or `opus-4.8` for reviews of plans and implementations. Add `gpt-5.5` only as an extra independent perspective.
- Never use Haiku.

## Mechanics

- Use `gpt-5.5` through the `openai/codex-plugin-cc` plugin inside Claude Code. It adopts user-level configuration from `~/.codex/config.toml`.
- Avoid custom Bash wrappers. Use the plugin's built-in tools and skills.
- Use `/codex:review` for non-destructive, read-only code quality assessment. It supports `--base <ref>` for branch analysis.
- Use `/codex:adversarial-review` for skeptical design review that pressure-tests tradeoffs, auth, and reliability. Append custom focus text to steer the review.
- Use `/codex:rescue` as the default implementation path, not just for second passes.
- For parallel fix batches, launch multiple `/codex:rescue` tasks with explicit disjoint file lists, then one Claude review over the combined diff.
- Use `/codex:status`, `/codex:result`, and `/codex:cancel` to check, fetch, or abort asynchronous jobs when using `--background` on heavy tasks.
- Do not invoke `/codex:*` commands through Claude's `Skill(...)` tool. Run them as Claude Code slash commands; `Skill(codex:review)` fails because that plugin skill disables model invocation.
- Run Claude models (`sonnet-5`, `opus-4.8`, `fable-5`) through the Agent/Workflow `model` parameter.

## GPT-5.5 In Workflows

- Have subagents and automated workflows call the plugin's native slash commands directly.
- Do not use raw terminal wrappers for Codex delegation when the plugin can do it.
- For closed-loop quality assurance, keep the review gate enabled with `/codex:setup --enable-review-gate`. The stop hook challenges Claude output with Codex before finalizing.
