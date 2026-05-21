---
title: "Rolling back to a previous deploy"
description: "Restore a prior deploy point including persistent volume state via ZFS snapshots. Rollback is atomic and instant; history of what happened remains intact."
llms:
  summary: "Restore a prior deploy point including persistent volume state via ZFS snapshots. Rollback is atomic and instant; history of what happened remains intact."
---
Rollback in Ployz restores the cluster to a previous deploy point: services return to their prior revision, and persistent volumes are reverted to the ZFS snapshot taken at that deploy's commit. Because ZFS snapshot revert is an in-place atomic operation, rollback does not need to re-transfer data — it is instant regardless of volume size.

Rollback is itself a deploy. It goes through the same plan → apply → commit phases, acquires the namespace deploy lock, and produces a new durable commit that records the rollback as an explicit fact. The cluster does not silently rewind — the history of what happened remains intact.

## Why rollback is safe

Ployz takes a ZFS snapshot at the commit point of every deploy. That snapshot captures the exact on-disk state at the moment traffic switched to the new version. Rolling back replaces the live dataset with the snapshot, returning the volume to the state it was in when that deploy became live.

Because the snapshot is taken atomically at the commit boundary — after the new containers started and before old instances were removed — the snapshot always corresponds to a known-good deployment state.

:::note
ZFS snapshots are copy-on-write. They consume negligible disk space until data diverges from the snapshot. Keeping recent deploy snapshots does not significantly increase storage usage.
:::

## The relationship between rollback and the deploy commit model

Every deploy commits routing and volume ownership facts as an immutable record. Rollback works against that record: you identify the deploy commit you want to restore, and Ployz uses it to reconstruct the prior service spec, placement, and volume state.

This means:

* You can roll back to any committed deploy, not just the most recent one.
* You cannot roll back past a volume move — a moved volume's prior location is no longer authoritative after the move commit.
* Cleanup failures after a commit (`FailedAfterCheckpoint` status) do not prevent rollback. The commit point itself is always valid.

## The `FailedAfterCheckpoint` status

If a deploy's cleanup phase fails after the final commit, Ployz records the status as `FailedAfterCheckpoint`. This status means:

* The new version is live. Traffic is already routing to it.
* Some old instances may not have been cleaned up.
* The deploy commit is durable and rollback-eligible.

`FailedAfterCheckpoint` is not a failed deploy from a traffic perspective. It is an operational signal that cleanup needs attention, but it does not erase the fact that the new version is running.

:::caution
Do not roll back in response to `FailedAfterCheckpoint` unless the new version itself is broken. Rolling back because of leftover old instances is unnecessary — those can be cleaned up explicitly without reverting the workload.
:::

## Identifying the deploy to roll back to

Use the deploy history to find the commit ID for the version you want to restore. Commits are listed in chronological order with their deploy ID, namespace, and status:

```bash
# List recent deploy commits for a namespace (JSON output for scripting)
ployzctl deploy preview -f current.toml --json
```

:::tip
Keep your manifests in version control alongside your code. The manifest hash is recorded in every deploy commit, so you can correlate a deploy ID back to the exact manifest that produced it.
:::

## Performing a rollback

Rollback is expressed as a deploy against the prior manifest. The simplest path is to re-deploy the previous manifest version:

### Identify the prior manifest

  Retrieve the manifest that produced the deploy you want to restore. If you store manifests in version control, check out the version corresponding to the target deploy commit.

  ```bash
  git show HEAD~1:deploy.toml > rollback.toml
  ```

### Preview the rollback deploy

  Verify the rollback plan before applying. The preview will show which services and volumes will revert.

  ```bash
  ployzctl deploy preview -f rollback.toml
  ```

  Confirm that the participating machines are reachable and that the target volumes have a valid snapshot to restore from.

### Apply the rollback

  Run the deploy. Ployz will stop the current instances, revert the ZFS snapshots, start the prior-revision containers, and commit the rollback as a new deploy fact.

  ```bash
  ployzctl deploy -f rollback.toml
  ```

  The command blocks until the rollback completes or fails. Traffic switches back at the commit point.

### Verify the result

  Confirm that the prior service revision is running and that routing is correct.

  ```bash
  ployzctl machine ls
  ployzctl deploy preview -f rollback.toml
  ```

  A clean preview showing no changes confirms that the cluster matches the rollback manifest.

## Atomicity guarantees

Rollback follows the same commit model as any other deploy:

* Before the rollback commit, failure leaves the cluster in its current state. Nothing has been reverted.
* At the rollback commit, routing flips to the prior revision and volume ownership transfers atomically.
* After the rollback commit, the prior version is live. Cleanup of the reverted instances follows the same cleanup-failure rules as any other deploy.

There is no intermediate state where some services are on the old version and others are on the new version within the same phase. The commit is all-or-nothing for the facts it contains.

## Next steps

### [Deploy phases](/operations/deploy)

  Understand the commit boundary that makes rollback safe and predictable.

### [Branch and promote](/operations/branch-and-promote)

  Use branching to test changes in isolation before promoting, reducing the need for rollback.
