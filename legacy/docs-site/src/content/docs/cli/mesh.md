---
title: "ployzctl mesh — manage the network"
description: "Create, join, inspect, start, stop, and destroy WireGuard overlay mesh networks. Also available as the 'network' alias for all subcommands."
llms:
  summary: "Create, join, inspect, start, stop, and destroy WireGuard overlay mesh networks. Also available as the 'network' alias for all subcommands."
---
The `mesh` command manages the WireGuard overlay network that connects cluster machines. Each mesh network gives machines a flat, encrypted overlay with stable IP addresses independent of the underlying physical network. Ployz owns the mesh end-to-end, which is what makes single-command primitives like `machine add` and `migrate` possible.

```bash
ployzctl mesh <subcommand> [flags]
ployzctl network <subcommand> [flags]  # alias
```

:::note
`mesh` and `network` are aliases for the same command. Both work identically.
:::

## Subcommands

### list — list mesh networks

  List all mesh networks known to the daemon, including their names and current state.

  ```bash
  ployzctl mesh list
  ```

  The response includes each network's name and state string. Use `--json` to get the full structured payload.

  ```bash
  ployzctl --json mesh list
  ```

### status — show status of a network

  Show the detailed status of a named mesh network, including the local machine's overlay IP and lifecycle state.

  ```bash
  ployzctl mesh status <network>
  ```

  - `network` (`string`) required:

    Name of the mesh network to inspect.
  

  The response includes the network name, the local machine's overlay IP, and the lifecycle string.

### join — join a mesh network

  Join the local machine to an existing mesh network using a join token. The token can be passed directly or provided on stdin.

  ```bash
  ployzctl mesh join [--token <token>] [--token-stdin]
  ```

  - `--token` (`string`):

    The join token string. Obtain this from the coordinator machine or from a cluster invite.
  

  - `--token-stdin` (`boolean`):

    Read the join token from stdin instead of the `--token` flag. Useful in pipelines where the token should not appear in the process list.
  

  :::note
    Provide either `--token` or `--token-stdin`, not both.
  :::

### ready — check if the mesh is ready

  Check whether the mesh is ready to accept workloads. Reports readiness, the current phase, store health, sync connectivity, and whether the workload subnet is present.

  ```bash
  ployzctl mesh ready [--json]
  ```

  - `--json` (`boolean`):

    Print the full readiness payload as JSON. Note: this is a subcommand-level `--json` flag in addition to the global one.
  

  This command exits with code 0 if the mesh is ready and a non-zero code if it is not, making it suitable for use in readiness probes and startup scripts.

  ```bash
  ployzctl mesh ready && echo "mesh is up"
  ```

### create — create a new mesh network

  Create a new named mesh network. The network is initialized on the local machine and ready for other machines to join.

  ```bash
  ployzctl mesh create <network>
  ```

  - `network` (`string`) required:

    Name for the new mesh network.
  

### init — initialize a new mesh

  Initialize a mesh network, optionally reading the network name from stdin. This is the lower-level initialization primitive used when the network name is generated or piped from another command.

  ```bash
  ployzctl mesh init [<network>] [--name-stdin]
  ```

  - `network` (`string`):

    Name of the mesh network to initialize. Optional if `--name-stdin` is set.
  

  - `--name-stdin` (`boolean`):

    Read the network name from stdin. Cannot be combined with a positional network argument.
  

### start — start a mesh network

  Start a previously created or stopped mesh network.

  ```bash
  ployzctl mesh start <network>
  ```

  - `network` (`string`) required:

    Name of the mesh network to start.
  

### stop — stop a mesh network

  Stop the running mesh network. WireGuard interfaces are brought down and peer connectivity is severed.

  ```bash
  ployzctl mesh stop [--force]
  ```

  - `--force` (`boolean`):

    Force-stop the mesh even if workloads are still running. Without `--force`, the daemon may refuse to stop if active services depend on the overlay network.
  

  :::caution
    Stopping the mesh network severs connectivity between cluster nodes. Running workloads that depend on overlay networking will lose peer access. Stop workloads before stopping the mesh in production.
  :::

### destroy — destroy a mesh network

  Permanently destroy a mesh network, removing all associated configuration and state. This is irreversible.

  ```bash
  ployzctl mesh destroy [<network>] [--name-stdin]
  ```

  - `network` (`string`):

    Name of the mesh network to destroy. Optional if `--name-stdin` is set.
  

  - `--name-stdin` (`boolean`):

    Read the network name from stdin.
  

  :::caution
    `mesh destroy` is permanent. All state associated with the network is removed. Machines connected to this network will lose cluster connectivity.
  :::

### self-record — print this machine's mesh record

  Print the local machine's mesh membership record. This includes the machine's overlay IP, public key, endpoints, and region role. Useful for debugging connectivity and for sharing the record with other machines during manual cluster setup.

  ```bash
  ployzctl mesh self-record
  ```

  Use `--json` to get the full structured `MachineMembership` record.

  ```bash
  ployzctl --json mesh self-record
  ```

## Examples

```bash
# Create a new mesh network named 'prod'
ployzctl mesh create prod

# Check mesh readiness (exits 0 if ready)
ployzctl mesh ready

# Show the status of the 'prod' network
ployzctl mesh status prod

# List all networks
ployzctl mesh list

# Join a network using a token
ployzctl mesh join --token eyJ...

# Join using a token from stdin (avoids token in process list)
echo "$MESH_TOKEN" | ployzctl mesh join --token-stdin

# Print this machine's mesh record
ployzctl --json mesh self-record

# Stop the mesh (requires no active workloads)
ployzctl mesh stop

# Destroy the mesh network permanently
ployzctl mesh destroy prod

# Use the network alias
ployzctl network list
```
