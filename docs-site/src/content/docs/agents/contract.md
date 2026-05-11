---
title: Agent Contract
description: How agents should drive Ployz safely.
llms:
  summary: Agent-safe command usage rules.
---

Ployz is designed for humans and coding agents. Agents should use the same
foreground command model as human operators.

Rules:

- Prefer foreground commands over hidden reconciliation.
- Use global `--json` for full daemon responses.
- Inspect `payload.kind` before parsing a response.
- Branch on structured payloads and exit codes, not display text.
- Retry only after inspecting operation records or explicit failure details.
- Verify after every mutation before planning the next command.

Useful current commands:

```bash
ployzctl --json status
ployzctl image operation get <id>
```
