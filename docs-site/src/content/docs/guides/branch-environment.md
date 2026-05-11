---
title: Branch an Environment
description: Environment branching as a north-star primitive.
llms:
  summary: Branching direction and current status.
---

Environment branching is a north-star primitive. The goal is to fork an
environment, including datasets, volumes, secrets, and routing, as one atomic
operation.

:::caution
This command is a product target, not a current executable command.
:::

```text
# Product target, not executable unless listed in Current Commands.
ployzctl branch <env>
```

This guide is intentionally directional until the command exists in the current
surface. Current lower-level deploy and storage pieces should not be documented
as a user-facing replacement for the primitive unless they are safe and
supported as a workflow.
