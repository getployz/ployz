---
title: "How Ployz models a cluster"
description: "Every node is a peer. NATS carries control-plane state. Intent, Status, and Observation are kept separate. No master node, no reconciler, no background mutations."
llms:
  summary: "Every node is a peer. NATS carries control-plane state. Intent, Status, and Observation are kept separate. No master node, no reconciler, no background mutations."
---
A Ployz cluster is a flat mesh of peer machines. There is no master node. No machine holds state that others lack, and no machine's removal breaks coordination. You can remove any node — including the one you are currently connected to — without a quorum ceremony or a controller migration. This peer model is what makes `machine remove` safe by construction.

## Every node is a peer

When a machine joins the cluster it receives:

* A WireGuard identity (public key and overlay IPv6 address).
* A NATS leaf node connection to the cluster's control-plane store.
* A subnet for workload container networking.
* A machine ID, region, and optional availability zone.

From that point forward, the machine is a full participant. Coordination, locking, and state visibility work through NATS on a peer-oriented model. No machine is elected leader for the cluster as a whole; authority is scoped to deploy namespaces, not to the cluster globally.

:::note
The `storage` flag on a machine controls whether it participates in NATS JetStream quorum. Nodes with `storage=false` still join the mesh and run workloads — they just do not hold durable control-plane state. See [Storage](/concepts/storage) for ZFS implications.
:::

## NATS as the control-plane substrate

NATS is not a message bus bolted on for convenience. It is the native substrate for everything the control plane needs to do:

* **Durable facts.** Deploy commits, machine membership records, routing events, and instance status are stored in NATS JetStream streams and KV buckets. These survive daemon restarts.
* **Coordination.** Deploy leases, participant locks, and quorum decisions happen through NATS. A command that cannot acquire a lock fails loudly; it does not queue or retry silently.
* **Request/reply commands.** Small participant actions — start a container, probe readiness, confirm a volume transfer — use NATS request/reply on per-machine subjects. No responder or timeout is an immediate foreground failure.
* **Ordered routing events.** The gateway and DNS service consume a NATS stream of routing events and rebuild their view from it. If freshness becomes uncertain, they reload rather than serving stale projections.

:::caution
NATS data-directory backups contain TLS private keys and ACME account credentials in plaintext. Treat any storage-enabled node — and any backup of its NATS store directory — as trusted with the cluster's secrets.
:::

## Three kinds of truth

Ployz separates state into three categories that are never mixed:

| Kind            | What it represents                                               | Examples                                                                    |
| --------------- | ---------------------------------------------------------------- | --------------------------------------------------------------------------- |
| **Intent**      | What an operator explicitly asked the cluster to do              | Deploy commits, machine membership records, instance status, routing events |
| **Status**      | Durable lifecycle facts emitted by operations                    | Deploy phase records, volume movement evidence, branch lineage              |
| **Observation** | Live reachability, health, and capacity checked at decision time | Placement probes, WireGuard handshake state, participant readiness          |

Intent and Status live in NATS JetStream — they are durable and survive restarts. Observation is always checked live, at the moment a decision needs to be made. The cluster does not rewrite Intent from stale Observations.

:::tip
When you ask `ployzctl` what the cluster looks like, you get Intent and Status from durable storage plus live Observations made at that moment. There is no cached "desired state" that might be stale.
:::

## Namespaces and machine membership

Workloads are grouped into **namespaces**. A namespace is the unit of deploy authority: one owning authority accepts durable deploy writes for a namespace, and routing events belong to that authority.

Machines are members of the cluster itself, not of any particular namespace. A single machine can run workloads from multiple namespaces. Placement decisions — which machines receive which workloads — happen at deploy time, based on live machine capacity and region role.

## Region roles and topology

Every machine has a **region** and an optional **availability zone**. These are operator-assigned topology labels used to guide placement decisions. Regions have one of four roles:

| Role        | Placement behavior                                         |
| ----------- | ---------------------------------------------------------- |
| `home_data` | Receives new placements; preferred for stateful workloads  |
| `compute`   | Receives new placements; preferred for stateless workloads |
| `draining`  | No new placements; existing workloads drain off            |
| `disabled`  | No new placements; excluded from all placement decisions   |

Region roles affect where workloads land. They do not create separate write authorities. Deploy commits, instance records, and routing events always belong to the namespace's owning authority regardless of which region a machine is in.

## Scale target: 1–200 nodes

Ployz targets clusters in the 1–200 node range. This is not an arbitrary limit — it is the range in which an operator can understand the whole system, explain every workload's placement, and reason about a migration or branch operation end-to-end.

### Single developer machine

  `ployzctl dev` runs the full cluster model locally. All primitives — branch, migrate, rollback — work identically to a multi-node cluster.

### Small office or bare-metal fleet

  Up to 200 nodes joined into one WireGuard mesh. One model, one set of primitives, no operational bifurcation between "dev" and "production".

Kubernetes is the right tool for 10,000-node fleets. Ployz is the right tool when you want an operator — human or agent — who can hold the entire cluster model in working memory and make decisions with complete information.
