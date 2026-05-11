---
title: ZFS Storage
description: ZFS is product strategy, not a hidden implementation detail.
llms:
  summary: Storage primitives and why ZFS matters.
---

ZFS is central to Ployz because it makes state operations cheap and explicit:

- snapshots,
- clones,
- incremental sends,
- receives,
- rollback.

Branching, volume forking, workload migration, and rollback are product
capabilities built on those mechanics. Storage capabilities should be visible
in the product surface rather than hidden behind a generic abstraction.

Btrfs can serve smaller-machine tiers with explicit migration paths, but ZFS is
the primary strategy for stateful cluster primitives.
