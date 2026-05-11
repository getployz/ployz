---
title: Daemon and Data Plane
description: ployzd is disposable; the data plane should keep serving last good state.
llms:
  summary: Control-plane and data-plane boundary.
---

`ployzd` is a disposable control-plane process. It can crash, restart, or
upgrade without disrupting the data plane.

The data plane includes:

- workload containers,
- WireGuard mesh,
- NATS,
- gateway,
- DNS,
- storage datasets and volumes.

On startup, the daemon adopts what is already running rather than recreating
managed infrastructure from scratch. Sidecars may be adopted, repaired, or
recreated by explicit operation, but they are not treated as ephemeral children
of one daemon process.
