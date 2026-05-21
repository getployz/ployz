---
title: "ployzctl migrate — move workloads"
description: "Move a running service and its persistent volumes from one cluster machine to another using ZFS incremental send. Includes preview and manifest rendering."
llms:
  summary: "Move a running service and its persistent volumes from one cluster machine to another using ZFS incremental send. Includes preview and manifest rendering."
---
The `migrate` command moves a workload — container and persistent volumes — from one machine to another within the cluster. Migration uses ZFS incremental send to transfer volume state efficiently, with minimal downtime. It is a single explicit operation with a clear result: the workload is running on the target machine or the command fails cleanly.

```bash
ployzctl migrate <subcommand> <service-ref> --to <machine> [flags]
```

:::note
Migration transfers both the container configuration and any persistent ZFS volumes attached to the service. The source volumes are snapshotted, sent to the target incrementally, and the workload is restarted on the target. Always preview before applying in production.
:::

## Identifying a service

The `service-ref` positional argument identifies the workload to migrate. It takes the form `namespace/service-name`, for example `production/web` or `staging/worker`.

## Subcommands

### preview — preview a migration

  Compute the migration plan without executing it. Returns what would be transferred, which volumes are involved, and the target machine. Use this before any production migration.

  ```bash
  ployzctl migrate preview <service-ref> --to <machine>
  ```

  - `service-ref` (`string`) required:

    The service to migrate, in `namespace/service` format.
  

  - `--to` (`string`) required:

    ID or address of the target machine to migrate the workload to.
  

  The preview response includes the target machine, the service's current placement, and the list of volumes that will be transferred.

### apply — execute a migration

  Execute the migration. The daemon stops the workload on the source machine, transfers its volumes using ZFS incremental send, and starts it on the target machine.

  ```bash
  ployzctl migrate apply <service-ref> --to <machine>
  ```

  - `service-ref` (`string`) required:

    The service to migrate, in `namespace/service` format.
  

  - `--to` (`string`) required:

    ID or address of the target machine.
  

  :::caution
    `migrate apply` stops the service on the source machine before transferring state. There will be a brief outage during the volume transfer and restart. For services with large volumes, the transfer time depends on the amount of changed data since the last snapshot.
  :::

### render-manifest — render the migration manifest

  Render the manifest that describes the migration operation, without applying it. Useful for auditing, storing in version control, or applying manually.

  ```bash
  ployzctl migrate render-manifest <service-ref> --to <machine>
  ```

  - `service-ref` (`string`) required:

    The service to migrate, in `namespace/service` format.
  

  - `--to` (`string`) required:

    ID or address of the target machine.
  

  Outputs the manifest to stdout.

## How migration works

1. The daemon snapshots all ZFS volumes attached to the service on the source machine.
2. The snapshot is sent to the target machine using ZFS incremental send over the cluster's WireGuard overlay network.
3. Once the transfer is complete, the workload is stopped on the source and started on the target from the received volumes.
4. The cluster membership and routing records are updated to reflect the new placement.

Because ZFS send is incremental, migrating a service that was previously migrated (or whose volumes were cloned from the target) transfers only the delta since the last common snapshot — making repeated migrations between the same pair of machines fast.

## Recommended workflow

### Preview the migration

  Confirm the source placement, target machine, and volumes involved.

  ```bash
  ployzctl migrate preview production/worker --to machine-b
  ```

### Drain the source machine (optional)

  If migrating because of planned maintenance, drain the source first to stop new workload placements.

  ```bash
  ployzctl machine drain machine-a
  ```

### Apply the migration

  Execute the migration. The daemon returns when the workload is running on the target.

  ```bash
  ployzctl migrate apply production/worker --to machine-b
  ```

### Verify

  Confirm the service is running on the expected machine.

  ```bash
  ployzctl --json machine ls
  ```

## Examples

```bash
# Preview migrating the 'worker' service in production to machine-b
ployzctl migrate preview production/worker --to machine-b

# Apply the migration
ployzctl migrate apply production/worker --to machine-b

# Get the migration manifest as JSON
ployzctl --json migrate render-manifest production/worker --to machine-b
```
