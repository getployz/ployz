---
title: "Migrating workloads between machines"
description: "Move a running service and its persistent volumes from one machine to another using ZFS incremental send. Always safe to preview before applying."
llms:
  summary: "Move a running service and its persistent volumes from one machine to another using ZFS incremental send. Always safe to preview before applying."
---
`ployzctl migrate` moves a workload — the container and any attached persistent volumes — from its current machine to a target machine. The transfer uses ZFS incremental send, which means only changed blocks travel over the wire. The workload's data arrives at the target before the container restarts there, so the cutover window is minimal.

Migration is always safe to preview before applying. The preview shows exactly what will move, which machines are involved, and what the resulting placement will look like. No changes happen until you explicitly apply.

## How migrate works

Ployz models a migration as a deploy: it resolves a placement plan that places the service on the target machine, moves the volume via ZFS send, starts the new container, commits the routing fact, and removes the old instance. The same deploy phases and commit boundary apply.

The key difference from a regular deploy is that volume movement is a first-class part of the plan. Ployz stops writes to the volume on the source machine, performs the ZFS transfer, starts the workload on the target, and commits volume ownership atomically. Only verified transfer success becomes durable evidence in the control plane.

## Subcommands

| Subcommand                | Description                                                 |
| ------------------------- | ----------------------------------------------------------- |
| `migrate preview`         | Compute the migration plan and print it; make no changes    |
| `migrate apply`           | Execute the migration end-to-end                            |
| `migrate render-manifest` | Print the deploy manifest that `migrate apply` would submit |

All three subcommands take the same arguments:

```bash
ployzctl migrate <subcommand> <service-ref> --to <machine-id>
```

| Argument      | Description                                                                |
| ------------- | -------------------------------------------------------------------------- |
| `service-ref` | The service to migrate, in `namespace/service` form (positional, required) |
| `--to`        | Target machine ID (required)                                               |

## Typical migration workflow

### Preview the migration

  Verify the plan before committing to anything. The preview shows the source and target machines, which volumes will be transferred, and the expected deploy phases.

  ```bash
  ployzctl migrate preview production/api --to machine-b
  ```

  Review the output carefully. Confirm that the target machine has enough disk capacity for the volume transfer.

### Check the rendered manifest (optional)

  If you want to inspect or audit the underlying deploy manifest before applying, render it:

  ```bash
  ployzctl migrate render-manifest production/api --to machine-b
  ```

  This prints the exact manifest that will be submitted to the deploy engine. You can save it and submit it manually with `ployzctl deploy -f` if you need finer control.

### Apply the migration

  Execute the migration. Ployz will acquire the namespace deploy lock, transfer volumes, start the workload on the target, commit ownership, and remove the old instance.

  ```bash
  ployzctl migrate apply production/api --to machine-b
  ```

  The command blocks until the migration completes or fails. Progress and any errors are printed as they occur.

### Verify the result

  Confirm that the service is running on the target machine and that routing is correct.

  ```bash
  ployzctl machine ls
  ployzctl deploy preview -f deploy.toml
  ```

## Volume transfer details

Ployz uses ZFS incremental send for volume migration. The sequence is:

1. Take a snapshot of the volume on the source machine.
2. Send the snapshot (or an incremental diff from a prior shared snapshot) to the target machine via ZFS receive over the cluster's WireGuard network.
3. Stop writes to the source volume.
4. Send a final incremental snapshot to minimize the data-loss window.
5. Start the workload on the target using the received volume.
6. Commit the volume ownership change as a durable fact.

The commit step is the point of no return. Before commit, the migration can be aborted and the source volume is still intact. After commit, the target owns the volume and the source instance is cleaned up.

:::note
ZFS transfer progress is live evidence tracked while the transfer runs. Only verified, successful transfer becomes durable evidence in the deploy commit. If the transfer fails before commit, the source volume is not affected.
:::

## What gets migrated

* The **workload container**: the service stops on the source and starts on the target using the same image and configuration.
* **Persistent volumes**: all ZFS volumes attached to the service are transferred incrementally.
* **Routing**: after commit, the gateway and DNS resolve the service at its new location. The overlay IP does not change.

:::caution
Migrate does not move secrets or environment variables stored outside the cluster's control plane. If your service reads secrets from an external store, verify that the target machine can reach that store before migrating.
:::

## Next steps

### [Managing machines](/operations/machines)

  Drain a machine before migrating all its workloads off, then remove it.

### [Deploy](/operations/deploy)

  Understand how the deploy commit boundary protects migrations from partial state.
