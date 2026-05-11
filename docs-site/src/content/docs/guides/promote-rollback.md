---
title: Promote and Roll Back
description: Promotion and rollback as explicit operations.
llms:
  summary: Promotion and rollback direction.
---

Promotion and rollback should be explicit operations with durable evidence.

:::caution
These command forms are product targets, not current executable commands.
:::

```text
# Product targets, not executable unless listed in Current Commands.
ployzctl promote <branch>
ployzctl rollback
```

Promotion should switch traffic to a prepared environment. Rollback should
restore the previous deploy point, including state when the substrate can
honestly support it.

Until these commands exist in the current surface, docs should describe the
semantics without presenting them as executable quickstart steps.
