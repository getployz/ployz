---
title: "Networking and mesh configuration"
description: "How Ployz builds a WireGuard overlay mesh, assigns node subnets, routes traffic through the gateway, resolves cluster DNS, and transfers volumes between nodes."
llms:
  summary: "How Ployz builds a WireGuard overlay mesh, assigns node subnets, routes traffic through the gateway, resolves cluster DNS, and transfers volumes between nodes."
---
Ployz networking is built on two layers: a WireGuard overlay mesh that connects every node at the IP level, and a set of cluster services — gateway, DNS, and NATS — that run on top of the overlay. This page explains how those layers are configured and how they interact.

***

## WireGuard overlay mesh

Each node in a Ployz cluster runs a WireGuard interface (`wg0`) and gets a unique subnet carved from the cluster's address range. Workload containers and sidecars bind to addresses inside that subnet, and they can reach any other node's addresses directly over the encrypted tunnel.

### Address allocation

Two fields in `config.toml` control the address space:

| Field               | Default         | Description                                            |
| ------------------- | --------------- | ------------------------------------------------------ |
| `cluster_cidr`      | `10.101.0.0/16` | The full address range shared by all nodes in the mesh |
| `subnet_prefix_len` | `24`            | The prefix length of each node's slice of that range   |

With the defaults, a `/16` range split into `/24` subnets gives 256 possible nodes and 254 usable addresses per node. To support more nodes, widen the CIDR. To give each node more addresses, decrease the prefix length.

:::caution
`cluster_cidr` must be the same on every node in a mesh. Changing it after a mesh has been initialized requires destroying and re-creating the mesh. Set it before running `ployzctl mesh init`.
:::

### Endpoint ordering

When a node advertises its network addresses to peers, Ployz filters and orders them according to a fixed policy. The ordering matters because it becomes the candidate order WireGuard uses for endpoint selection and rotation:

1. **Dropped entirely:** loopback, link-local, IPv6 ULA, interfaces below the minimum MTU for the overlay, and container, bridge, or helper interfaces that are not cluster-facing.
2. **Ordered by likely usefulness:**
 * Private RFC1918 addresses first
 * CGNAT addresses second
 * Public addresses after that

Public-IP discovery is folded into the same ordering. Directly routable private paths are preferred over broader internet paths, but NAT-discovered public reachability is still advertised when needed.

:::note
You do not configure endpoint ordering directly. It is applied automatically when a node publishes or refreshes its endpoint record.
:::

***

## Mesh commands

Use `ployzctl mesh` to manage the lifecycle of a mesh network.

### ployzctl mesh init

  Create a new mesh network on this node and activate it as the current network. Pass a name as the argument, or use `--name-stdin` to read it from standard input.

  ```bash
  ployzctl mesh init my-cluster
  ```

  After `init`, the node generates a WireGuard keypair, allocates a subnet from `cluster_cidr`, and writes the network record to the store.

### ployzctl mesh create

  Create a named network record without activating it. Useful when you want to set up a network before starting it.

  ```bash
  ployzctl mesh create staging
  ```

### ployzctl mesh start

  Start the WireGuard interface and sidecars for an existing network.

  ```bash
  ployzctl mesh start my-cluster
  ```

### ployzctl mesh stop

  Stop the active mesh. Pass `--force` to stop even if workloads are still running.

  ```bash
  ployzctl mesh stop
  ployzctl mesh stop --force
  ```

### ployzctl mesh join --token

  Join this node to an existing mesh using an invite token generated on the primary node. The token encodes the network's public key, CIDR, and initial peer endpoints.

  ```bash
  ployzctl mesh join --token "eyJ..."
  # or read from stdin
  ployzctl mesh join --token-stdin
  ```

  After joining, the daemon connects to NATS through the overlay, syncs routing state, and begins receiving peer updates.

### ployzctl mesh status / list

  Inspect active network state:

  ```bash
  ployzctl mesh list           # list all known networks
  ployzctl mesh status my-cluster  # detailed peer and subnet state
  ployzctl mesh ready          # exit 0 if mesh is healthy, 1 otherwise
  ployzctl mesh ready --json   # machine-readable health report
  ```

***

## NATS control-plane

NATS is the native substrate for cluster coordination. It provides durable key-value records, streams for ordered events, request/reply for foreground commands, and work queues for distributed operations.

NATS is managed as a sidecar by the daemon — you do not run or configure it separately. The daemon starts NATS when the mesh starts and adopts a running NATS process on restart if the configuration matches.

:::note
NATS is not a messaging bus for application workloads. It is the internal control-plane substrate. Application services communicate over the overlay network using whatever protocols their workloads require.
:::

The data plane — WireGuard tunnels, the gateway, DNS, NATS, and running containers — continues to serve last good state when `ployzd` is absent. Daemon restart does not disrupt running workloads or break mesh connectivity.

***

## Gateway

The HTTP/HTTPS gateway runs as a sidecar on each node and proxies inbound traffic to the correct workload container based on routing rules published to the cluster store.

Configure the gateway through the daemon's `config.toml`:

| Field                       | Default      | Description                                 |
| --------------------------- | ------------ | ------------------------------------------- |
| `gateway_listen_addr`       | `0.0.0.0:80` | HTTP listen address                         |
| `gateway_https_listen_addr` | *(unset)*    | HTTPS listen address — enables TLS when set |
| `gateway_threads`           | `2`          | Worker threads for the gateway process      |

### HTTPS support

When `gateway_https_listen_addr` is set, the gateway serves TLS. Certificates are loaded from the cluster's routing store using SNI-based selection. You can also supply static certificate paths via the gateway's environment variables (`PLOYZ_GATEWAY_TLS_CERT_PATH` and `PLOYZ_GATEWAY_TLS_KEY_PATH`), but both paths must be set together with `gateway_https_listen_addr`.

```toml
# config.toml
gateway_listen_addr       = "0.0.0.0:80"
gateway_https_listen_addr = "0.0.0.0:443"
gateway_threads           = 4
```

:::tip
Increase `gateway_threads` on nodes that serve high request volumes. A value between 2 and the number of physical cores is a reasonable starting point.
:::

***

## Cluster DNS

Each node runs a DNS sidecar that answers queries for cluster service names. Services deployed to Ployz are automatically registered in the cluster DNS and are reachable by name from any node in the mesh.

The DNS server listens on the node's overlay IPv6 address on port 53. In Docker runtime mode, it may also bind a bridge address so containers in the `ployz-networking` namespace can resolve cluster names.

You do not configure cluster DNS directly in `config.toml`. The daemon provisions and configures the DNS sidecar automatically based on the active network and the node's overlay address. To expose DNS metrics, set `dns_metrics_listen_addr` in `config.toml` or the `PLOYZ_DNS_METRICS_LISTEN_ADDR` environment variable.

***

## ZFS transfer port

When a volume is migrated between nodes, the daemon opens a direct TCP connection from the destination node to the source node to stream the ZFS dataset. The source node listens on `zfs_transfer_port` for these connections.

| Field               | Default | Override                                           |
| ------------------- | ------- | -------------------------------------------------- |
| `zfs_transfer_port` | `4319`  | `PLOYZ_ZFS_TRANSFER_PORT` or `--zfs-transfer-port` |

Ensure that port `4319` (or your configured value) is reachable between cluster nodes on the overlay network. The transfer always uses the overlay address, not the public IP.

### Linux

  If you run a host firewall such as `nftables` or `iptables`, allow inbound TCP on the transfer port for overlay addresses:

  ```bash
  # Allow ZFS transfer from overlay addresses (example: 10.101.0.0/16)
  nft add rule inet filter input ip saddr 10.101.0.0/16 tcp dport 4319 accept
  ```

### macOS

  On macOS, the daemon runs on the host and bridges to the Docker VM. ZFS volumes are not supported in the Docker runtime, so the transfer port is unused in the default macOS configuration.

***

## macOS networking architecture

On macOS, `ployzd` runs on the host. The WireGuard interface, NATS, gateway, DNS, and all workload containers run inside the Docker Desktop Linux VM. The daemon bridges the two environments:

```
macOS host                         Docker Desktop VM
+----------------+                 +------------------------------+
| ployzd daemon  |                 | ployz-networking container   |
|                |  WG bridge      |   wg0 overlay interface      |
| OverlayBridge  +---------------->|                              |
|                |                 | nats-server                  |
| NATS bridge    +---------------->| ployz-gateway                |
|                |                 | ployz-dns                    |
|                |                 | workload containers          |
+----------------+                 +------------------------------+
```

`OverlayBridge` uses userspace WireGuard and a smoltcp TCP stack to bridge the macOS host into the container overlay network. NATS, gateway, and DNS bind on the node's overlay IPv6 address so other mesh nodes can reach them directly.

:::note
macOS requires OrbStack or Docker Desktop to be running. The daemon will not start the mesh if Docker is not reachable.
:::
