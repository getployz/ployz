---
title: "Primitives, not policies"
description: "Ployz exposes eight explicit operations with bounded effects and clear results, instead of a reconciler that continuously rewrites cluster state behind the scenes."
llms:
  summary: "Ployz exposes eight explicit operations with bounded effects and clear results, instead of a reconciler that continuously rewrites cluster state behind the scenes."
---
Ployz is built around a single design conviction: small-cluster infrastructure gets better when the system gives you explicit operations, not a policy engine that manages itself. Every change to the cluster happens because you ran a command. There are no controllers running in the background, no autoscalers silently mutating placement, and no desired-state document waiting to be reconciled. What you see is what is true — right now, on demand.

## What makes something a primitive

A primitive in Ployz has four properties:

* **Single command.** The entire operation is one `ployzctl` invocation. If you find yourself writing a script to combine multiple commands for a routine task, that task is a missing primitive.
* **Bounded effect.** The operation touches a clearly defined set of resources. You can know, before running it, what will change.
* **Clear result.** The command either succeeds and returns verifiable facts, or fails cleanly. Ambiguous progress — half-applied state reported as success — is the worst possible outcome.
* **Safe to retry.** Failed operations leave enough state for the operator or an agent to retry without reasoning about hidden partial progress.

:::note
A primitive fails loudly when preconditions are missing. It does not silently succeed with partial work, and it does not wait for eventual consistency to catch up.
:::

## Contrasting with the Kubernetes model

Kubernetes is a declarative system. You describe a desired state, and controllers continuously reconcile live state toward it. This is the right model for 10,000-node fleets where no human can track every decision.

For small clusters — the 1–200 node range Ployz targets — that tradeoff inverts. You do not need the cluster to reason for itself. You need operations that are easy to inspect, honest when they cannot complete, and tractable for both humans and automation. Reconcilers add operational surface without adding capability at small scale.

### Kubernetes

  Declare desired state. Controllers converge live state toward it in the background. Operators observe convergence and reason about reconciler behavior.

### Ployz

  Run one command. It inspects preconditions, executes bounded steps, and returns a result. The cluster does not change state in the background.

## Why primitives make automation tractable

The same discipline that makes a CLI useful for humans makes it usable for agents. An agent can:

<figure class="diagram">
  <img src="/assets/operator-loop.svg" alt="The operator loop: observe live state, choose one operation, execute bounded foreground work, verify the evidence, decide the next step." />
  <figcaption>Humans and agents run the same loop: observe, choose, execute, verify.</figcaption>
</figure>

1. Inspect the current cluster state.
2. Choose a primitive that achieves the desired outcome.
3. Run it and observe the structured result.
4. Verify the outcome and decide what comes next.

There is no reconciler behavior to predict, no eventual consistency window to wait for, and no hidden state machine mutating things underneath the agent's plan.

:::tip
If you are automating Ployz — with a script, a CI system, or a coding agent — you can treat each primitive as an atomic unit: run it, read the result, branch on success or failure.
:::

## The eight primitives

### machine add

  Provisions a fresh machine into the cluster. The machine joins the WireGuard mesh, receives a NATS leaf connection, and becomes available for workload placement. The command does not complete until the machine is reachable and active.

  ```bash
  ployzctl machine add --network <network> user@<host>
  ```

### machine rm

  Drains workloads off a machine, transfers their persistent state to surviving machines, and removes it from the cluster. Safe regardless of which machine is removed — there is no special node whose removal breaks the cluster. Use `--force` to skip online cleanup for an unreachable machine.

  ```bash
  ployzctl machine rm <machine-id>
  ```

### migrate

  Moves a workload — including its persistent state — from one machine to another. Uses ZFS incremental send to transfer volumes. Preview before applying with `migrate preview`.

  ```bash
  ployzctl migrate apply <namespace/service> --to <machine-id>
  ```

### branch

  Forks an entire namespace — services, volumes, routing — as a single atomic operation. The branch is an independent copy backed by ZFS copy-on-write clones, so it is instant regardless of data size. Services and volumes can each be `branch` (copy-on-write) or `fresh` (new empty).

  ```bash
  ployzctl branch apply <source-namespace> <target-namespace>
  ```

### promote

  Promotion is done through the deploy path: deploy the branched service spec into the production namespace. Traffic flips atomically at the commit point. The prior namespace remains snapshotted for rollback.

  ```bash
  ployzctl deploy -f promote.toml
  ```

### rollback

  Restores the previous deploy point, including state. Re-deploy the previous manifest to revert services and ZFS volume snapshots atomically. The rollback is itself a deploy — it produces a new commit recording the revert.

  ```bash
  ployzctl deploy -f rollback.toml
  ```

### fork-volume

  Volume forking is handled through the `branch` operation with `--volume-mode branch`. ZFS copy-on-write clones are used to fork volumes instantly — the clone and origin share unchanged blocks until they diverge.

  ```bash
  ployzctl branch apply <source> <target> --volume-mode branch
  ```

### dev (local cluster)

  `ployzctl` with the `docker` runtime runs the same cluster model locally on a developer machine. The daemon, mesh, NATS, gateway, and DNS all start inside Docker Desktop's Linux VM. The primitives — branch, migrate, rollback — work identically to a multi-node cluster.

  ```bash
  # Install with Docker runtime (default on macOS)
  ployzctl daemon install --runtime docker --service-mode user
  ployzctl mesh init dev-local
  ```

## Primitives compose into workflows

These eight operations are the building blocks. If your workflow — deploy a PR branch, test it, promote to production, clean up on merge — requires no shell scripting, the primitives are doing their job. If you find yourself chaining commands to achieve something routine, that workflow is a signal that a primitive is missing.
