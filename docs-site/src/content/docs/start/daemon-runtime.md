---
title: Daemon Runtime
description: Understand how ployzctl and ployzd work together.
llms:
  summary: Relationship between ployzctl and ployzd.
---

`ployzctl` is the operator entrypoint. Today it has one native command:

```bash
ployzctl daemon install
```

Other arguments are forwarded to the sibling `ployzd` binary. In practice, the
current `ployzd` command surface is also the current `ployzctl` operator
surface.

```bash
ployzctl --json status
ployzctl machine ls
ployzctl deploy --file ployz.toml --dry-run
```

Global daemon flags are available through the forwarded surface:

```bash
ployzctl --config ./ployz.toml --socket /tmp/ployz.sock --json status
```

## Output modes

- `--json` prints the full daemon response for scripting and agents.
- `--plain` prints compact human-readable text.
- `--quiet` suppresses success output where possible.

Prefer `--json` for automation.
