---
name: opus-advisor
description: Run Claude Opus as a scarce, read-only advisor for Ployz implementation plan gates and frozen-diff review gates. Use before implementing a ticket and when mirroring the project's Spec, Standards, thermo-nuclear, and ponytail review lanes through Opus.
---

# Opus Advisor

Keep Codex as supervisor, implementer, integrator, and verifier. Use Opus only
for judgment-heavy plan and review gates. Give Opus `Read`, `Grep`, and `Glob`
so it can verify the packet against the repository; keep mutation, publication,
and agent-spawning tools unavailable.

## Invoke Opus

Create a self-contained packet in a temporary file and pass it on stdin from the
repository root. Codex owns complete discovery and evidence gathering; Opus may
inspect repository files to verify the packet.

For plans and self-contained review packets:

```sh
claude -p --remote-control --model opus --effort high --safe-mode \
  --tools Read,Grep,Glob --allowedTools Read,Grep,Glob \
  --permission-mode dontAsk --no-session-persistence \
  --prompt-suggestions false --output-format json < "$PACKET"
```

For the thermo-nuclear cold read, use the same read-only repository tools so
Opus can load the local skill named by the minimal prompt and inspect relevant
code:

```sh
claude -p --remote-control --model opus --effort max --safe-mode \
  --tools Read,Grep,Glob --allowedTools Read,Grep,Glob \
  --permission-mode dontAsk --no-session-persistence \
  --prompt-suggestions false --output-format json < "$PACKET"
```

The thermo packet contains the frozen diff and only this instruction:

```text
Read .agents/skills/thermo-nuclear-code-quality-review/SKILL.md.
Review the supplied diff range and report back your findings.
```

Do not paste design rationale, suspected findings, or extra steering into that
packet.

## Validate every response

Parse the JSON result. Require runtime `modelUsage` to identify an Opus model;
the alias or the response's prose is not proof. Treat authentication, transport,
missing usage, wrong model, malformed JSON, or an invalid verdict as
`advisor unavailable`.

For a plan, require the first non-empty line of the result to be exactly:

```text
PLAN_APPROVED
PLAN_REVISE
```

`PLAN_REVISE` lists only material gaps and one concrete correction per gap.
Codex adjudicates the advice and may request one confirmation after a material
revision.

For a review, require each lane's first non-empty line to be exactly:

```text
REVIEW_PASS
REVIEW_BLOCK
```

`REVIEW_BLOCK` cites the file and line or requirement, states the material
impact, and gives the smallest correction. Codex verifies and dispositions
every finding.

## Build packets

The plan packet includes the ticket intent, acceptance criteria, relevant
repository facts and constraints, Codex's plan, risks, and verification.

Freeze one review SHA. Each review packet includes that SHA, base, commit list,
ticket or spec, applicable standards or lane instructions, and the frozen diff.
Do not ask Opus to rediscover context.

Use one consolidated packet with four separated verdicts for a small change.
For a large or risky change, use four fresh calls and `--effort max`, one for
each of Spec, Standards, thermo-nuclear, and ponytail. Never reuse a Claude
session between lanes.
