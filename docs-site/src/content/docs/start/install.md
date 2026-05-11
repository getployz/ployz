---
title: Install
description: Install or reconfigure the local Ployz daemon runtime.
llms:
  summary: Install command and runtime selection.
---

`ployzctl daemon install` installs or reconfigures the local daemon runtime.
It reports the runtime, service backend, config path, and control socket.

```bash
ployzctl daemon install --runtime docker --service-mode user
```

For host-managed service installation:

```bash
ployzctl daemon install --runtime host --service-mode system
```

## Runtime choices

| Runtime | Service mode | Use when |
| --- | --- | --- |
| `docker` | `user` | You want a local Docker-backed runtime for development or a small machine. |
| `host` | `user` | You want host-backed processes without system service management. |
| `host` | `system` | You want system-managed daemon and sidecar services. |

Use `--install-manifest <PATH>` when you need to write the install result to a
specific manifest path.
