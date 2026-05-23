---
title: "Managing machines in your cluster"
description: "Add, update, drain, and remove machines from a Ployz cluster. Covers the full machine lifecycle: init, add, invite, storage promotion, standby, and rm."
llms:
  summary: "Add, update, drain, and remove machines from a Ployz cluster. Covers the full machine lifecycle: init, add, invite, storage promotion, standby, and rm."
---
Machines are the compute units of a Ployz cluster. Every machine runs `ployzd` and participates in the WireGuard mesh, the NATS control plane, and the workload runtime. You add and remove machines with explicit commands — there is no autoscaler and no background scheduler placing machines for you.

## Listing machines

```bash
ployzctl machine ls
```

This shows every machine in the cluster, including its lifecycle state, overlay IP, region role, and authority posture. Use `--json` for structured output suitable for scripting.

## Initializing a new machine

`machine init` provisions and joins a single machine in one step. Use this when you have SSH access to a fresh host and want to bring it into an existing network:

```bash
ployzctl machine init user@10.0.1.20 \
--network my-cluster \
--runtime host \
--service-mode system \
--install-source release \
--install-version latest
```

| Flag                | Description                                              |
| ------------------- | -------------------------------------------------------- |
| `--network`         | Name of the cluster network to join (required)           |
| `--runtime`         | Container runtime: `docker` or `host`                    |
| `--service-mode`    | How `ployzd` runs: `user` or `system`                    |
| `--install-source`  | Where to pull the binary from: `release` or `git`        |
| `--install-version` | Specific release version to install                      |
| `--install-git-url` | Git remote URL (used with `--install-source git`)        |
| `--install-git-ref` | Git ref to build from (used with `--install-source git`) |

## Adding machines

`machine add` joins one or more machines into the cluster. It is the multi-target variant of `machine init`, and accepts the same install flags plus an optional SSH identity file:

### Run machine add

  Pass one or more SSH targets. Ployz will provision each in sequence.

  ```bash
  ployzctl machine add \
    --network my-cluster \
    --runtime host \
    --service-mode system \
    user@10.0.1.21 user@10.0.1.22
  ```

### Verify the result

  Check that each new machine is visible and in the `active` lifecycle state.

  ```bash
  ployzctl machine ls
  ```

### Check RTT (optional)

  Confirm that WireGuard peering is healthy by measuring round-trip times between machines.

  ```bash
  ployzctl machine rtt
  ```

:::tip
Pass `--identity` to specify an SSH private key file when the target requires a non-default identity.
:::

## Machine invite workflow

Use invites when you cannot provide SSH credentials to `machine add` — for example, when a new machine is bootstrapping itself into the cluster. The invite token is short-lived and single-use.

**On the cluster coordinator:**

```bash
# Create an invite token (default TTL: 600 seconds)
ployzctl machine invite create --ttl-secs 300

# List active invites
ployzctl machine invite list

# Revoke an invite before it is used
ployzctl machine invite revoke <invite-id>
```

**On the joining machine:**

```bash
# Import the token; the machine joins the cluster network
ployzctl machine invite import --token <token>
```

The invite is consumed on import. Once a machine has joined, it appears in `machine ls` and can receive workloads.

## Updating machines

`machine update` upgrades the `ployzd` binary on one or more machines to a target version:

```bash
# Update specific machines
ployzctl machine update --version 0.4.2 <machine-id-1> <machine-id-2>

# Update all machines to the latest release
ployzctl machine update
```

The command reports which machines updated successfully and which failed, so partial failures are always visible.

## Lifecycle transitions

Machines move through an explicit lifecycle. You control transitions with three commands:

### activate

  Marks the machine as ready to accept new workload placements.

  ```bash
  ployzctl machine activate <machine-id>
  ```

### drain

  Stops new placements to this machine. Existing workloads keep running until you migrate or remove them.

  ```bash
  ployzctl machine drain <machine-id>
  ```

### standby

  Removes the machine from active placement without draining running workloads immediately.

  ```bash
  ployzctl machine standby <machine-id>
  ployzctl machine standby --force <machine-id>
  ```

:::note
`machine standby --force` bypasses the precondition check that normally prevents a machine from entering standby while it still holds workloads. Use this when you need to park a machine that already has its workloads migrated manually.
:::

## Promoting storage authority

By default, only a subset of machines hold the full NATS control-plane store. `machine storage promote` adds machines to the storage authority set and configures the replication policy:

```bash
ployzctl machine storage promote \
--replicas 3 \
<machine-id-1> <machine-id-2> <machine-id-3>
```

Machines must be `active` and on a compatible version. The command reports each promotion result individually, so you can see exactly which targets succeeded and which did not.

:::caution
Storage-enabled nodes are trusted with the cluster's control-plane secrets, including TLS private keys and ACME account keys. Treat storage machines the same as you would a root CA host.
:::

## Removing a machine

`machine rm` removes a machine's membership record from the cluster:

```bash
ployzctl machine rm <machine-id>
```

By default, the command attempts to clean up the machine's on-device state — stopping workloads, leaving the mesh, and removing the daemon — before deleting the membership record.

```bash
# Skip online cleanup and remove only the membership record
ployzctl machine rm --force <machine-id>
```

:::note
`--force` skips all online target cleanup. Use it when the machine is unreachable or already destroyed and you need to remove the stale membership record from the cluster.
:::

Before removing a machine, drain its workloads first. Ployz will not automatically migrate workloads on removal — that decision belongs to you.

## Next steps

### [Deploy workloads](/operations/deploy)

  Deploy services to your cluster after machines are joined and active.

### [Migrate workloads](/operations/migrate)

  Move a running workload and its persistent volumes to a different machine.
