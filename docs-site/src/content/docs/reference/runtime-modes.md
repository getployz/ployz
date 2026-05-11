---
title: Runtime Modes
description: Runtime target and service mode combinations.
llms:
  summary: Runtime target matrix.
---

The public runtime surface is split across runtime target and service mode.

| Runtime target | Service mode | Meaning |
| --- | --- | --- |
| Docker | User | Docker-backed mesh, store, and sidecars with user-managed services. |
| Host | User | Host-backed mesh and sidecars without system service management. |
| Host | System | Host-backed runtime with system-managed sidecars. |

Memory runtimes are test-only and should not shape public operator docs.
