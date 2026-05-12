---
title: "Ployz: cluster orchestration built on explicit primitives"
description: "Ployz is a primitive orchestration core for small clusters. Deploy, migrate, branch, and roll back with single commands that complete or fail clearly."
llms:
  summary: "Ployz is a primitive orchestration core for small clusters. Deploy, migrate, branch, and roll back with single commands that complete or fail clearly."
---
Ployz turns cluster operations into explicit, atomic commands. Instead of writing manifests and waiting for reconcilers, you run `ployzctl deploy`, `ployzctl branch`, or `ployzctl migrate` — each command completes or fails cleanly, every time. Ployz targets clusters in the 1–200 node range: small enough to reason about end-to-end, large enough to matter.

### [Quickstart](/quickstart)

  Go from a fresh machine to a running cluster in under five minutes.

### [Installation](/installation)

  Install ployzctl and the daemon on Linux or macOS.

### [Core Concepts](/concepts/primitives)

  Understand what makes Ployz different from Kubernetes and other orchestrators.

### [CLI Reference](/cli/overview)

  Every ployzctl command, flag, and option, documented with examples.

## What Ployz does

Ployz gives you a small set of strong operations — primitives — that compose into any workflow you need. There are no controllers, no autoscalers, and no background reconcilers silently rewriting cluster state.

### [Machine operations](/operations/machines)

  Add and remove machines. Drain workloads before removal.

### [Deploy](/operations/deploy)

  Deploy workloads from a manifest or inline flags. Preview before applying.

### [Branch & promote](/operations/branch-and-promote)

  Fork an environment atomically for a PR, then promote it to production.

### [Migrate](/operations/migrate)

  Move a workload and its persistent state to a different machine.

## How it works

### Install ployz

  Run the one-line installer to get `ployzctl` and the `ployzd` daemon on your machine.

  ```bash
  curl -fsSL https://ployz.sh | bash -s -- install
  ```

### Create a cluster network

  Initialize a WireGuard mesh network that your machines will join.

  ```bash
  ployzctl mesh init my-cluster
  ```

### Add machines

  Provision additional machines into your cluster with a single command.

  ```bash
  ployzctl machine add --network my-cluster user@192.168.1.10
  ```

### Deploy your first workload

  Deploy a container image to your cluster.

  ```bash
  ployzctl deploy -f deploy.toml
  ```

:::note
Ployz runs on Linux and macOS. On macOS, the daemon uses the Docker runtime (Docker Desktop or OrbStack required). On Linux, you can use either the Docker or host runtime.
:::
