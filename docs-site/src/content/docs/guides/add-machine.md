---
title: Add a Machine
description: Add capacity to a small Ployz cluster.
llms:
  summary: Machine add guide skeleton.
---

`machine add` provisions fresh capacity into the cluster.

Current command shape:

```bash
ployzctl machine add <target>...
```

Useful adjacent commands:

```bash
ployzctl machine ls
ployzctl machine invite create
ployzctl machine rtt
```

Before adding a machine, know which runtime and service mode it should use.
After adding it, verify membership and reachability with current machine
inspection commands.
