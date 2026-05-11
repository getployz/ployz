---
title: Get Started
description: Start with the open-core Ployz CLI and daemon.
llms:
  summary: Orientation for the open-core quickstart.
---

Start with the daemon and CLI. The current `ployzctl` binary implements
`daemon install` directly and forwards operator commands to the sibling
`ployzd` binary.

The quickest useful path is:

1. Install or reconfigure the local daemon runtime.
2. Inspect current daemon state with structured output.
3. Choose a current command from the forwarded `ployzd` surface.
4. Verify the result before running the next operation.

:::note
These docs distinguish current executable commands from north-star product
primitives. Do not paste a north-star command into automation unless it appears
in [Current Commands](/commands/current/).
:::

## Current safe first commands

```bash
ployzctl daemon install --runtime docker --service-mode user
ployzctl --json status
ployzctl doctor
ployzctl machine ls
```

Some commands require a running daemon and local configuration. If a transport
error says the daemon socket is missing, start or install the daemon runtime
before continuing.
