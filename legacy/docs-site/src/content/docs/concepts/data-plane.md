---
title: "The Ployz data plane"
description: "Workloads, WireGuard, NATS, the gateway, and DNS keep running when the daemon restarts. The daemon is disposable; the data plane is durable."
llms:
  summary: "Workloads, WireGuard, NATS, the gateway, and DNS keep running when the daemon restarts. The daemon is disposable; the data plane is durable."
---
Ployz separates the control plane — the daemon, `ployzd`, that handles commands and coordinates operations — from the data plane, which is the set of services that keep serving traffic regardless of daemon state. If `ployzd` crashes, upgrades, or restarts, your workloads keep running, WireGuard tunnels stay up, NATS keeps serving state, the gateway keeps proxying, and DNS keeps resolving. The daemon is disposable. The data plane is durable.

## Data plane components

### Workload containers

  Docker containers running your application code. Never restarted by the daemon. They outlive any number of daemon restarts.

### WireGuard mesh

  The encrypted overlay network connecting all nodes. Each machine gets an overlay IPv6 address and a subnet for workload containers. Tunnels persist through daemon restarts.

### NATS

  The control-plane substrate. Serves durable cluster state (deploy commits, machine membership, routing events) to the daemon and participates in JetStream quorum.

### ployz-gateway

  The HTTP/HTTPS reverse proxy. Rebuilds its routing table from durable NATS state on startup and then consumes ordered routing events. Does not serve stale projections silently.

### ployz-dns

  The DNS resolver for cluster-internal names. Like the gateway, it rebuilds from durable state on startup and consumes routing events to stay current.

### Storage datasets and volumes

  ZFS datasets (or Btrfs on smaller machines) backing persistent volumes. Data persists independently of daemon state. See [Storage](/concepts/storage) for details.

## Restart behavior by component

When `ployzd` starts, it follows an adopt-first lifecycle for every managed infrastructure component. It does not blindly restart everything it owns.

| Component                                            | Restart behavior                                                              |
| ---------------------------------------------------- | ----------------------------------------------------------------------------- |
| Workloads                                            | Never touched by daemon restart                                               |
| Gateway (`ployz-gateway`)                            | Adopted if running and config matches; recreated on drift                     |
| DNS (`ployz-dns`)                                    | Adopted if running and config matches; recreated on drift                     |
| NATS                                                 | Adopted if running and parent network namespace unchanged; recreated on drift |
| WireGuard                                            | Adopted if healthy                                                            |
| CLI RPC, remote deploy, background command listeners | Ephemeral — always restarted with the daemon                                  |

The practical effect: a daemon restart is invisible to your workloads and to traffic. The daemon comes back, adopts what is running, and resumes handling commands. No downtime, no routing blip.

## What "adopt" means

Adoption is not a passive check. For each managed infrastructure component, the daemon:

1. **Inspects** what is already running on this node.
2. **Compares identity** against the full expected specification — not just whether the process is alive, but whether it matches the configuration the daemon would have created.
3. **Adopts** the component without touching it if identity matches.
4. **Recreates** the component with visible status if it is missing or has drifted.

:::note
"Drift" means the running component's identity no longer matches what Ployz would create fresh. A NATS server in a different network namespace, or a gateway container with a different config hash, will be recreated rather than adopted.
:::

## Docker labels and container identity

For Docker-backed deployments, Ployz uses container labels to encode identity. Two labels are central to the adopt/recreate decision:

* **`ployz.config-hash`** — A stable hash of the full configuration the container was created with. If this hash matches the hash the daemon would produce today, the container is adopted.
* **`ployz.parent-container-id`** — For NATS and sidecars, the ID of the networking container that the process shares a network namespace with. If the parent has changed, the child is recreated.

These labels let the daemon reconstruct identity from the running container without external state. If a container has no Ployz labels, it is not managed by Ployz and is never touched.

## macOS and the Docker runtime

On macOS, `ployzd` runs on the host while the data plane runs inside Docker Desktop's Linux VM. The daemon uses an `OverlayBridge` — a userspace WireGuard tunnel backed by a smoltcp TCP stack — to bridge the macOS host into the container overlay network.

```text
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

NATS, gateway, and DNS bind on the node's overlay IPv6 address so other mesh nodes can reach them directly. In the Docker runtime they share the `ployz-networking` container's network namespace to access the `wg0` interface.

## Why this design matters

The disposable-daemon model has a direct operational consequence: deploying a new version of `ployzd` is a zero-downtime operation. You stop the old daemon, start the new one, and the data plane keeps serving traffic throughout. The daemon misbehaving — crashing in a loop, hanging, or being killed — cannot brick the data plane. Workloads stay up, NATS stays up, and routing stays live.

:::caution
The adopt-first lifecycle depends on the daemon being able to inspect running infrastructure. If you manually remove containers or interfaces that Ployz manages, the daemon will recreate them on next startup — which may cause a brief interruption for that specific component.
:::
