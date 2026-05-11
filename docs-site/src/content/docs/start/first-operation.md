---
title: First Operation
description: Run a non-mutating first operation and verify the current command surface.
llms:
  summary: First safe commands after install.
---

Start with non-mutating commands. They tell you whether the daemon can be
reached and what the cluster currently knows.

```bash
ployzctl --json status
ployzctl doctor
ployzctl machine ls
```

Then choose a specific operation from [Current Commands](/commands/current/).
For example, preview a deploy manifest before applying it:

```bash
ployzctl deploy --file ployz.toml --dry-run
```

Or inspect a service migration without applying it:

```bash
ployzctl migrate preview service:web --to machine-a
```

:::caution
Bare `ployzctl deploy` is not a useful quickstart command. Current deploy flows
need either a manifest path or a specific subcommand such as `deploy service`.
:::
