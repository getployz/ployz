---
title: "ployzctl machine — manage cluster nodes"
description: "Add, remove, inspect, and lifecycle-manage Ployz cluster nodes. Covers listing, RTT probing, initialization, storage promotion, invites, and machine state transitions."
llms:
  summary: "Add, remove, inspect, and lifecycle-manage Ployz cluster nodes. Covers listing, RTT probing, initialization, storage promotion, invites, and machine state transitions."
---
The `machine` command is the primary surface for managing the physical and virtual nodes that make up a Ployz cluster. Each node is a peer — there is no master. Machine operations are explicit commands with visible preconditions and structured results. They do not trigger background reconciliation.

```bash
ployzctl machine <subcommand> [flags]
```

:::note
All machine operations communicate with the running `ployzd` daemon over the local Unix socket. Ensure the daemon is running before issuing machine commands.
:::

## Subcommands

### ls / list — list machines

  List all machines currently known to the cluster, including their lifecycle state, overlay IP, region, and role.

  ```bash
  ployzctl machine ls
  ployzctl machine list
  ```

  The response includes each machine's ID, lifecycle, authority posture, region, region role, overlay IP, optional subnet, and creation timestamp. Use `--json` to get the full structured payload for scripting.

  ```bash
  ployzctl --json machine ls
  ```

### rtt — round-trip time between machines

  Probe round-trip latency between all machine pairs in the cluster. Reports median and standard deviation in milliseconds.

  ```bash
  ployzctl machine rtt
  ```

  Any reachability warnings (e.g., machines that could not be probed) are included in the `warnings` field of the response. Use `--json` to surface them in scripts.

### init — initialize a remote machine

  Initialize a remote machine and join it to the cluster over SSH. The target must be reachable via SSH from the local machine. The daemon installs itself on the remote host and joins the named network.

  ```bash
  ployzctl machine init <target> --network <network-name> [flags]
  ```

  **Positional arguments**

  - `target` (`string`) required:

    SSH target for the remote machine, e.g. `user@host` or `192.168.1.10`.
  

  **Flags**

  - `--network` (`string`) required:

    Name of the mesh network the machine should join.
  

  - `--runtime` (`docker | host`):

    Container runtime to configure on the remote machine. If omitted, the daemon uses its compiled default.
  

  - `--service-mode` (`user | system`):

    Whether the daemon is installed as a user-level or system-level service on the remote machine.
  

  - `--install-source` (`release | git`):

    Source to install the daemon from. `release` pulls a published release artifact; `git` builds from source.
  

  - `--install-version` (`string`):

    Version string to install when using `--install-source release`.
  

  - `--install-git-url` (`string`):

    Git repository URL to clone when using `--install-source git`.
  

  - `--install-git-ref` (`string`):

    Git ref (branch, tag, or commit SHA) to check out when using `--install-source git`.
  

### add — add machines to the cluster

  Add one or more already-running machines to the cluster. The targets must already have `ployzd` installed and running. Unlike `init`, `add` does not SSH in to install the daemon.

  ```bash
  ployzctl machine add [flags] <target> [<target>...]
  ```

  **Positional arguments**

  - `targets` (`string[]`) required:

    One or more machine targets to add. At least one target is required.
  

  **Flags**

  - `--identity` (`PATH`):

    Path to an SSH private key file to use when connecting to the target machines.
  

  - `--runtime` (`docker | host`):

    Container runtime target for the machines being added.
  

  - `--service-mode` (`user | system`):

    Service installation mode for the machines being added.
  

  - `--install-source` (`release | git`):

    Installation source. `release` uses a published artifact; `git` builds from source.
  

  - `--install-version` (`string`):

    Version to install when `--install-source release` is set.
  

  - `--install-git-url` (`string`):

    Git repository URL when `--install-source git` is set.
  

  - `--install-git-ref` (`string`):

    Git ref to check out when `--install-source git` is set.
  

  The response reports which machines joined successfully, which are awaiting self-publication, and which failed at each stage (preflight, join, self-record, ready, enable).

### storage promote — promote machines to storage role

  Promote one or more machines to the storage role, configuring them to participate in the replicated NATS control-plane store. This is an irreversible role assignment.

  ```bash
  ployzctl machine storage promote [--replicas <n>] <machine-id> [<machine-id>...]
  ```

  **Positional arguments**

  - `targets` (`string[]`) required:

    One or more machine IDs to promote. At least one is required.
  

  **Flags**

  - `--replicas` (`number`):

    The target replica count for the storage cluster after promotion.
  

  :::caution
    Promoting a machine to the storage role grants it access to all cluster-private material stored in NATS, including TLS keys and invite tokens. Only promote machines you control and trust.
  :::

### update — update machine daemon versions

  Update the `ployzd` daemon on one or more machines. If no machine IDs are provided, all machines in the cluster are updated.

  ```bash
  ployzctl machine update [--version <version>] [<machine-id>...]
  ```

  **Positional arguments**

  - `machine-id` (`string[]`):

    Zero or more machine IDs to update. If omitted, all cluster machines are targeted.
  

  **Flags**

  - `--version` (`string`):

    The daemon version to install on targeted machines.
  

  The response lists updated machines and any failures, each with the machine ID, version, and a message.

### activate — activate a machine

  Transition a machine from standby or drained state back to active. The machine begins accepting workload placements again.

  ```bash
  ployzctl machine activate <target>
  ```

  - `target` (`string`) required:

    Machine ID or address of the machine to activate.
  

### drain — drain workloads off a machine

  Signal that a machine should stop receiving new workload placements and transition existing workloads away. Use this before maintenance or removal.

  ```bash
  ployzctl machine drain <target>
  ```

  - `target` (`string`) required:

    Machine ID or address of the machine to drain.
  

  :::tip
    Run `ployzctl machine ls` after draining to confirm the machine has reached the drained lifecycle state before proceeding with removal or maintenance.
  :::

### standby — put a machine in standby

  Put a machine into standby mode. A machine in standby remains in the cluster membership but does not accept new workload placements.

  ```bash
  ployzctl machine standby <target> [--force]
  ```

  - `target` (`string`) required:

    Machine ID or address of the machine to put in standby.
  

  - `--force` (`boolean`):

    Force the standby transition without waiting for workloads to migrate away.
  

### rm — remove a machine

  Remove a machine from the cluster. By default, the daemon attempts to perform online cleanup (draining workloads, transferring state) before removing the membership record.

  ```bash
  ployzctl machine rm <id> [--force]
  ```

  - `id` (`string`) required:

    ID of the machine to remove.
  

  - `--force` (`boolean`):

    Skip online target cleanup and remove only the membership record from the cluster store. Use when the machine is unreachable or already destroyed.
  

  :::caution
    `--force` skips workload migration and state transfer. Any persistent volumes on the machine will be lost. Drain and migrate workloads first if their data matters.
  :::

## Invite subcommands

Invite tokens allow a machine to join a cluster without direct SSH access from the coordinator. The inviting machine creates a token with a TTL; the joining machine imports it.

### invite create — create an invite token

  Create a new invite token that a machine can use to join the cluster.

  ```bash
  ployzctl machine invite create [--ttl-secs <seconds>]
  ```

  - `--ttl-secs` (`number`):

    Time-to-live for the invite token in seconds. The token cannot be used after it expires.
  

### invite list — list pending invites

  List all pending invite tokens, including their IDs, expiry times, status, and which machine consumed them (if any).

  ```bash
  ployzctl machine invite list
  ```

### invite revoke — revoke an invite

  Revoke a pending invite token, preventing it from being used even if it has not yet expired.

  ```bash
  ployzctl machine invite revoke <invite-id>
  ```

  - `invite-id` (`string`) required:

    ID of the invite token to revoke.
  

### invite import — import an invite token

  Import an invite token on the joining machine. This causes the local daemon to use the token to join the cluster.

  ```bash
  ployzctl machine invite import --token <token>
  ```

  - `--token` (`string`) required:

    The invite token string to import. Obtain this from `invite create` on the coordinator machine.
  

## Examples

```bash
# List all machines
ployzctl machine ls

# Initialize a remote machine and join it to the 'prod' network
ployzctl machine init ubuntu@10.0.1.5 --network prod --runtime docker

# Add a machine that already has ployzd running
ployzctl machine add --identity ~/.ssh/id_ed25519 10.0.1.6

# Promote two machines to storage role with 3 replicas
ployzctl machine storage promote --replicas 3 machine-a machine-b machine-c

# Update all machines to the latest daemon version
ployzctl machine update

# Drain a machine before maintenance
ployzctl machine drain machine-a

# Force-remove an unreachable machine
ployzctl machine rm machine-a --force

# Create a 10-minute invite token
ployzctl machine invite create --ttl-secs 600

# Import an invite token on the joining machine
ployzctl machine invite import --token eyJ...
```
