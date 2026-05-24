---
title: Operator Perspective Commands With Corrosion Rows
date: 2026-05-24
category: docs/solutions/architecture-patterns/
module: cluster coordination
problem_type: architecture_pattern
component: control-plane
severity: high
applies_when:
  - Designing cluster-wide mutating commands
  - Choosing between replicated rows and peer RPC
  - Handling stale rows, partitions, and concurrent operators
  - Introducing Corrosion-backed state replication
tags:
  - corrosion
  - peer-rpc
  - operator-loop
  - replicated-rows
  - split-brain
  - control-plane
---

# Operator Perspective Commands With Corrosion Rows

## Context

Ployz operations are operator commands, not background reconciliation. An
operator connects to any reachable node, runs a primitive, and expects a clear
result. The coordinating node should use the cluster picture available at that
moment, verify the peers involved in the requested operation, run the concrete
work, and publish the outcome.

Corrosion is a strong fit for replicated rows and observations, but it should
not become the command execution path. A command needs bounded acceptance,
structured failure, live precondition checks, and direct responses from target
peers. Those are peer RPC responsibilities.

## Guidance

Treat the contacted daemon as the coordinator for the command. It reads the
Corrosion rows it currently has, probes or RPCs the peers involved in the
operation, computes a concrete plan, and executes that plan.

Use Corrosion for replicated state:

- machine membership and capabilities,
- placement inputs,
- rows emitted by completed lifecycle events,
- live observations that should be visible to other nodes,
- conflict evidence after partitioned peers exchange rows again.

Use bounded peer RPC for concrete work:

- preparing or starting workloads,
- stopping containers or changing local routing,
- snapshotting, cloning, or sending storage,
- probing reachability at decision time,
- applying local daemon state transitions that require a direct result.

Keep the peer RPC protocol narrow and internal. Public CLI/API requests must
not be deserialized directly as daemon-to-daemon commands. The internal
protocol should expose only the node-local operations a peer may perform for a
coordinator.

Do not require perfect global consensus before every operation. If the
coordinator cannot reach a required peer or prove a required precondition, the
command fails or proceeds only when the primitive explicitly supports degraded
behavior. If disconnected operators mutate disconnected halves of a cluster,
surface the conflict when rows meet again; the next operator command decides
how to resolve it.

Use narrow operation locks, leases, or command evidence only when a product path
proves it needs them. Most rows are owner-written and mostly static. Do not add
a coordination table or lock subsystem just because concurrent operators are
theoretically possible; first make the operation's owner and failure audience
explicit.

## Why This Matters

This preserves the product model: the cluster does not silently converge toward
policy in the background. It executes explicit primitives with visible
preconditions and visible outcomes.

Corrosion gives every node a fast shared picture of rows. RPC gives each
operation bounded foreground execution. Mixing the two responsibilities would
make command success ambiguous: a row write could look like work was accepted
before the target node actually did anything.

The split-brain posture is deliberately operational. Ployz should not pretend
small clusters have a perfect global view during partitions. It should make the
coordinator's view, reachability checks, and operation result legible so the
operator can decide the next command.

## When to Apply

- A new primitive needs to mutate state on one or more peers.
- A replicated row looks tempting as a command queue.
- A status surface combines durable rows with live reachability checks.
- A partition or stale row could make an operation unsafe.
- A daemon-to-daemon RPC payload is being designed.

## Examples

A deploy command reads current machine and workload rows from Corrosion,
probes candidate machines, computes placement, sends peer RPCs to prepare and
start the selected workload, and records durable lifecycle rows only at the
operation's real commit points. It does not wait for a reconciler to notice
desired state and eventually converge.

A machine remove command uses Corrosion to discover known workload ownership
and storage placement, but it still RPCs involved nodes to drain, transfer, and
verify concrete state. If a required peer is unreachable and the primitive does
not define degraded removal, the command fails visibly.

## Related

- `VISION.md` defines the operator loop, the primitive surface, and the rule
  that Corrosion rows are not command execution.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  covers the adjacent status-surface split between durable truth and live
  observation.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md`
  covers preflight and structured failure before mutating peers.
