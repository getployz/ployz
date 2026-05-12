---
title: "What is Ployz? A primitive orchestration core for clusters"
description: "Ployz turns cluster operations into explicit commands — add machines, deploy, branch, promote, roll back — without background reconcilers or controllers."
llms:
  summary: "Ployz turns cluster operations into explicit commands — add machines, deploy, branch, promote, roll back — without background reconcilers or controllers."
---
Ployz is a primitive orchestration core for clusters in the 1–200 node range. It gives operators a set of single-command operations — add a machine, deploy a workload, branch an environment, promote, roll back, migrate state — each with visible preconditions, a bounded effect, a clear result, and a way to verify what happened. There are no background reconcilers rewriting cluster state, no autoscalers, and no desired-state documents to keep in sync. The cluster does what you tell it, when you tell it, and reports back honestly.

## The problem it solves

Most small teams do not run hyperscale infrastructure. They run a handful of machines and need a few strong capabilities: join machines into one private network, deploy and route workloads, move workloads and persistent state, branch and promote environments, roll back cleanly, and diagnose problems when something goes wrong. Modern orchestrators like Kubernetes encode all of that behavior into policy engines — controllers, operators, admission webhooks, autoscalers — that are essential at 10,000 nodes and actively counterproductive at ten.

Ployz is designed around a different bet: small-scale infrastructure gets better when the system exposes real operational primitives instead of hiding them behind reconciler loops. The operator sees the cluster, decides what to do, runs a command, sees the result, and decides the next thing. That is the whole loop.

### [Quickstart](/quickstart)

  Go from zero to a running cluster with a deployed workload in minutes.

### [Installation](/installation)

  Install Ployz on Linux or macOS with the one-line installer or from source.

## Primitives, not policies

Policy belongs at decision time — in the operator's head, not in cluster manifests. Ployz exposes each infrastructure operation as a single command that completes or fails cleanly and is safe to retry.

| Command                                            | What it does                                                                    |
| -------------------------------------------------- | ------------------------------------------------------------------------------- |
| `ployzctl machine add`                             | Provision a fresh machine into the cluster                                      |
| `ployzctl machine rm`                              | Drain workloads, transfer state, remove from cluster                            |
| `ployzctl migrate apply <workload> --to <machine>` | Move a workload, including persistent state, between machines                   |
| `ployzctl branch apply <source> <target>`          | Fork an environment — services, volumes, routing — as a single atomic operation |
| `ployzctl deploy`                                  | Deploy a manifest; promotion is done by deploying into the target namespace     |
| `ployzctl deploy -f rollback.toml`                 | Restore a prior deploy point by re-deploying the previous manifest              |
| `ployzctl image distribute`                        | Clone an image across cluster nodes with ZFS copy-on-write semantics            |
| `ployzctl machine ls`                              | Inspect live machine state and cluster membership                               |

If you find yourself writing a script to combine multiple `ployzctl` commands to achieve a workflow, that workflow is a missing primitive.

## What Ployz is not

:::caution
Ployz does not target hyperscale. It is not a smaller Kubernetes, a reconciler with a friendlier UI, a managed PaaS, or a configurable toolkit you assemble into an orchestrator. If you need a 10,000-node fleet, Kubernetes is the right tool. Ployz targets clusters small enough that an operator can hold the whole system model in working memory.
:::

Ployz does not:

* Run autoscalers, controllers, or reconcilers.
* Maintain a desired-state document that background processes converge toward.
* Silently mutate cluster state between commands.
* Require special nodes or a master to hold cluster authority.

## Core components

Ployz is built around three components that work together.

### ployzd

  The daemon that runs on each machine. It is disposable: it can crash, upgrade, or restart without disrupting WireGuard tunnels, NATS, the gateway, DNS, or workload containers. On startup it adopts what is already running.

### ployzctl

  The operator CLI. Every operation in the primitive surface is a `ployzctl` subcommand. The CLI forwards most commands directly to `ployzd` via a Unix socket.

### Data plane

  The set of services that keep serving last good state when `ployzd` is absent: workload containers, WireGuard mesh, NATS, gateway, DNS, and storage datasets.

## How it compares to Kubernetes

|                     | Ployz                                 | Kubernetes                             |
| ------------------- | ------------------------------------- | -------------------------------------- |
| Target scale        | 1–200 nodes                           | Hundreds to tens of thousands          |
| State model         | Live state, reported on demand        | Desired state, continuously reconciled |
| Operations          | Explicit commands                     | Declarative manifests                  |
| Background activity | None — no controllers, no reconcilers | Controllers, operators, autoscalers    |
| Primitives          | Single-command operations             | Composable resource types              |
| Failure model       | Loud failure, safe retry              | Eventual convergence                   |

## The operator loop

The system is designed around a tight, human-readable loop:

1. You observe the cluster with `ployzctl status`.
2. You decide what to do.
3. You run one command.
4. The command completes or fails cleanly.
5. You verify the result.

There is no controller running ahead of you or behind you. There is no manifest to keep in sync. Every state change is an explicit operation triggered by an operator — or by automation acting on their behalf — with a clear return value.

:::tip
Ployz is equally usable by humans and by automation. The same primitives that make the CLI good for operators also make it tractable for agents and CI workflows: concrete actions, structured output, idempotent retry behavior, and explicit verification hooks.
:::

## Storage, network, and runtime

Ployz owns the substrate end-to-end, which is what makes single-command primitives possible.

* **ZFS** provides instant clone, atomic snapshot, and incremental send — the substrate for branching, migration, and rollback.
* **WireGuard** provides a flat, controllable overlay network across all cluster nodes.
* **Docker** (or host processes on Linux) provides container identity and isolation that Ployz controls directly.
* **NATS** is the control-plane substrate: durable facts, coordination, request/reply commands, and operator-visible state surfaces.

Every node is a peer. There is no master. Coordination and state visibility work on a peer-oriented model so that `machine remove` is safe regardless of which machine is removed.

## Local, self-hosted, and cloud

:::note
Ployz is open core. You can self-host on your own machines, drive it with `ployzctl` or any general-purpose coding agent, and run a local cluster on your Mac using the Docker runtime — without paying anyone. **ployz-cloud** is an optional hosted product built on top — a Railway-style UI with a built-in operator — for teams that want a managed experience. The primitives are identical in both cases.
:::

A developer running a local Ployz cluster on a Mac gets the same operations as a fleet operator running production workloads on bare metal. Branching, migration, and rollback work the same way. The model does not bifurcate between "dev mode" and "real mode."
