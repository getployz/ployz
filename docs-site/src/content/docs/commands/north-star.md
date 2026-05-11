---
title: North-Star Primitives
description: Product primitive names from the Ployz vision.
llms:
  summary: Planned product primitive surface.
---

The product vision names the primitive surface Ployz is growing toward:

:::caution
These are product targets. Do not treat a north-star spelling as executable
unless it also appears in [Current Commands](/commands/current/).
:::

```text
# Product targets, not executable unless listed in Current Commands.
ployzctl machine add
ployzctl machine remove
ployzctl migrate <workload> --to <machine>
ployzctl branch <env>
ployzctl promote <branch>
ployzctl rollback
ployzctl fork-volume
ployzctl dev
```

Some current commands already exist with different spellings. For example,
current removal is `ployzctl machine rm <id>`, while the product primitive name
is `machine remove`.
