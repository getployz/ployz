---
title: "Storage in Ployz: ZFS and volumes"
description: "ZFS is not an implementation detail — it is what makes branch, fork-volume, migrate, and rollback work as single commands. Btrfs is supported as a small-machine tier."
llms:
  summary: "ZFS is not an implementation detail — it is what makes branch, fork-volume, migrate, and rollback work as single commands. Btrfs is supported as a small-machine tier."
---
Ployz owns storage end-to-end. There is no generic "persistent volume" abstraction sitting between you and the filesystem. ZFS is the primary storage backend because its native capabilities — instant snapshots, copy-on-write clones, and incremental streaming send — are exactly the substrate that single-command primitives require. Without them, `branch`, `fork-volume`, `migrate`, and `rollback` would be multi-step procedures involving downtime and data copies. With ZFS, they are atomic operations.

## ZFS as primary backend

ZFS provides three capabilities that directly map to Ployz primitives:

### Snapshots

  Atomic, instant point-in-time captures of a dataset. The basis for `rollback` — promoting a branch leaves the old environment snapshotted and instantly restorable.

### Clones (copy-on-write)

  Instant forks of a snapshot that share unchanged blocks with their origin. The basis for `branch` and `fork-volume` — cloning a 500 GB database takes milliseconds, not hours.

### Incremental send

  Stream only the changed blocks between two snapshots to another machine. The basis for `migrate` — volumes transfer efficiently without a full copy.

### Quota enforcement

  Hard per-dataset quotas. Ployz sets a quota on each volume dataset. An `overcommit_ratio` controls how much of the pool's available space can be allocated across all volumes.

### ZFS configuration

Two ZFS-related settings control how Ployz uses the pool:

* **`zfs_root`** — The ZFS dataset under which Ployz creates all volume datasets. For example, `tank/ployz`. Ployz reads the mountpoint of this dataset at startup and creates child datasets beneath it.
* **`overcommit_ratio`** — A multiplier on the pool's available space used to calculate how much quota can be allocated across all volumes. A ratio of `1.5` allows total allocated quota to reach 1.5× the pool's actual free space. Set this based on how much you expect volumes to diverge from their allocated size.

:::note
The `overcommit_ratio` must be a finite positive number. Ployz rejects invalid values at startup rather than silently accepting them and failing later.
:::

## Btrfs: the small-machine tier

Btrfs is supported for machines where ZFS is unavailable or impractical — typically smaller machines, VMs with constrained kernel configurations, or environments where ZFS kernel modules cannot be loaded.

Btrfs provides the same snapshot and clone primitives that Ployz requires, so `branch`, `fork-volume`, and `rollback` work the same way from the operator's perspective. What changes is performance and reliability at scale: ZFS has a stronger track record for large datasets, higher-throughput transfer, and more predictable behavior under concurrent workloads.

:::tip
If you start on Btrfs and later want to move to ZFS, use `migrate` to move your volumes to a ZFS-backed machine. The transfer uses the same incremental-send mechanism either way.
:::

## Volumes and workload attachment

A **volume** in Ployz is a named dataset declaration in a deploy manifest. When a service mounts a volume, Ployz ensures the corresponding ZFS (or Btrfs) dataset exists and mounts it into the container at the specified path.

Volume declarations have a **scope**:

* **`single`** — The volume is attached to exactly one instance. Used for stateful workloads like databases where shared access would cause corruption. A service with `scope=single` and `replicas > 1` is rejected at manifest validation time.
* Other scopes — Shared or replicated volumes for workloads that can safely share access.

Volumes persist independently of the services that mount them. Removing a service does not remove its volumes. You remove volumes explicitly, or you move them with `migrate`.

## fork-volume: copy-on-write cloning

`fork-volume` creates an instant copy-on-write clone of a volume for use by another workload. The clone and the original share all unchanged blocks at the time of the fork. Only blocks written after the fork diverge, and only those blocks consume additional pool space.

```bash
ployzctl fork-volume <volume>
```

A common use case is branching a database for a staging environment. Instead of waiting for a multi-gigabyte copy, the fork completes in milliseconds. The staging environment gets its own isolated copy of the data and can write to it freely without affecting the original.

## ZFS transfer protocol for migration

When a volume moves between machines — during `migrate`, `machine remove`, or a deploy that relocates a stateful workload — Ployz uses a streaming ZFS send/receive protocol over TCP. The default transfer port is **4319**.

The transfer flow is:

1. Take a snapshot of the volume on the source machine.
2. Stream the snapshot (or the delta from a shared base snapshot) to the target machine using `zfs send | zfs receive`.
3. Once the transfer is verified, the deploy commits durable volume movement evidence: which deploy moved the volume, which machines were involved, and which snapshot GUID confirmed the transfer.
4. The workload starts on the target machine against the received dataset.

:::note
Only verified transfer success is folded into a deploy commit. A transfer that completes but cannot be verified does not become durable movement evidence, and the deploy does not proceed as if the move succeeded.
:::

The transfer uses incremental sends when the source and target already share a common snapshot — for example, after a previous partial migration. This makes repeat migrations (such as re-migrating after a failed deploy) significantly faster than a full resend.

## ZFS is product strategy

ZFS ownership is what makes the Ployz primitive surface possible. A managed PaaS abstracts storage away to serve many customers; Ployz keeps it because owning the substrate is the unlock for single-command operations.

Branch, snapshot, clone, send, and rollback are not features built on top of storage — they are properties of the storage layer itself, surfaced directly as product primitives. When you run `ployzctl branch`, you are not running a procedure that copies files. You are taking a ZFS snapshot and creating a clone. When you run `ployzctl rollback`, you are not re-deploying from source. You are restoring a known-good snapshot.

:::caution
ZFS pool health directly affects cluster reliability. Monitor pool status on storage-enabled nodes. A degraded pool does not automatically prevent Ployz operations, but a failed pool will make durable state on that node unavailable until the pool is repaired.
:::
