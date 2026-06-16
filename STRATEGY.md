---
name: Ployz
last_updated: 2026-06-16
---

# Ployz Strategy

## Target problem

Solo developers and small operators can start on one VPS, but scaling beyond one machine forces a jump to Kubernetes-level complexity or fragile manual operations. The hard part is updating, moving, and scaling services across machines with minimal downtime while keeping the system understandable and controllable.

## Our approach

Ployz is built around explicit, bounded commands, not a reconciler. A deploy inspects the runtime state, attempts the rollout, leaves evidence if it fails, and then stops; cluster state only changes when an owned operation is running.

## Who it's for

**Primary:** Solo developers and small technical operators - They use Ployz to get Railway/Vercel-style deploy ergonomics while keeping runtime control on their own hardware.

## Key metrics

- **Push-to-healthy time** - Time from push to healthy service, especially over high-latency links; measured from deploy operation events.
- **Regular deploy success rate** - Percent of deploys with valid, healthy inputs that complete successfully; measured from deploy outcomes.
- **Operation terminality** - Percent of mutating commands that reach exactly one clear terminal result; measured from operation state.
- **Failed deploy inspectability** - Percent of failed deploys where the operator can understand the failure from operation evidence before SSHing; measured through product telemetry and qualitative review.

## Tracks

### Explicit operations and evidence

The operation lifecycle, progress events, terminal outcomes, typed failures, and retained failed deploy evidence.

_Why it serves the approach:_ This makes every mutation inspectable and bounded, so failed commands leave useful evidence instead of hidden cluster behavior.

### Namespace deploy engine

Namespace revisions, runtime snapshots, deploy plans, phases, health gates, update order, promotion, and cleanup.

_Why it serves the approach:_ This is the core machinery that makes multi-machine deploys feel simple while preserving explicit control over every change.

### Machine and control-plane substrate

Machine identity, NATS services, JetStream KV and streams, durable workers, subject permissions, endpoint subnets, and dataplane projections.

_Why it serves the approach:_ The substrate gives Ployz reliable command, state, and operation ergonomics while keeping JetStream simple and disposable rather than treating it as unrebuildable cluster truth.

### Self-hosting ergonomics

Push-to-deploy, staging branches, snapshots, clones, volume movement, and Cloud/dashboard/SDK flows.

_Why it serves the approach:_ This delivers the Railway/Vercel feel on user-owned hardware, which is the experience users are choosing Ployz for.

## Milestones

- **2026-06-16** - Alpha is launched.
- **Undated near-term** - v1 with good dashboard integration.
- **Undated near-term** - Hacker News launch.

## Marketing

**One-liner:** Self-hosted Railway for small clusters.

**Key message:** Railway/Vercel ergonomics on hardware you control, without Kubernetes. Internally, Ployz is an explicit-operations orchestrator: no hidden reconciler, no silent cluster mutation, just bounded commands with durable evidence.
