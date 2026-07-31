# Vision

Ployz is a dead-simple, mesh-native control plane for small clusters: one
binary per machine, one shared converged store, one WireGuard mesh.

It is for the 1–200 machine range: homelabs, small teams, customer-owned
servers, bare metal, and modest VPS fleets. Past ~200 machines the answer is
cells — many clusters — never a bigger cluster.

The only metric is operability: install steps, day-2 surface, failure drills,
recovery command count, concepts in one head. Simplest wins. Future-proofing
is thin seams, never building ahead.

## Product Bet

Operating small infrastructure should not require operating a distributed
system first. Ployz has no core, no quorum, and no coordination point:

- Cluster config is rows in a shared store, converged to every machine.
- Each machine's Keeper converges that machine toward the rows it does not
  own, and reports into status rows nobody else may write.
- Commands are row writes plus watching status rows, accepted by any machine.

The cluster should not surprise the operator. If something changed, a row
changed, and the row names its writer.

## Experience Goals

Ployz should feel:

- terse,
- observable,
- hard to accidentally misuse,
- safe to retry,
- honest when uncertain,
- easy to automate,
- small enough to hold in your head.

Failures are part of the product. A failed deploy should leave useful
evidence, not erase the scene. A stale machine should be visible as stale,
not silently converted into truth.

## Architecture Shape

One `ployzd` binary per machine, plus a stock, version-pinned Corrosion
sidecar as a plain systemd unit. Machines connect over a pluggable WireGuard
mesh — builtin WireGuard or Tailscale — with cryptokey routing as identity.
The API is HTTP/JSON with SSE watches, served by every machine over the mesh.
No RPC framework, no message broker.

Membership is a roster row plus the mesh peer set. Machines are admitted by
SSH provisioning or a revocable multi-use join token; Ployz Cloud mints
tokens as an ordinary mesh peer. Namespace isolation is policy on the flat
mesh, never topology. Ingress and caller identity sit behind thin seams:
ingress provider (direct, Cloudflare Tunnel, Tailscale Funnel) and Principal.

## Consistency Thesis

Converged beats coordinated.

Every machine holds the whole cluster's config. A write accepted anywhere
converges everywhere; no machine's loss blocks commanding the rest. The worst
failure class this product recognizes is a control plane the operator must
size, back up, or repair before they can command their own machines.

The price, stated plainly: writes merge last-writer-wins. Under concurrent or
partitioned writes, an earlier config command can silently lose to a later
one. For a single operator that conflict means racing yourself inside a
sub-second convergence window — rare, and priced in. Rows carry writer and
timestamp so a fold can be surfaced after the fact; Ployz does not build
coordination machinery to prevent one.

The trust ceiling: membership is write authority. Admitting a machine trusts
it with the cluster's config, so admission is the security decision.
Operator-signed rows are deferred until hostile-edge or multi-tenant demand
is real; the additive schema and per-row writer identity keep that retrofit
open.

`docs/architecture/backbone.md` turns this thesis into review checks.

## State Model

Rows, each with one writer:

- Config rows are operator decisions, written through any machine's API and
  converged everywhere. Keeper enforces them; it never authors them.
- Status rows are machine testimony. Each machine writes only its own.
- Docker is execution reality. Status rows report it, never replace it.

Freshness is visible — mesh last-handshake age and row timestamps — and
never inferred. A command against an unreachable machine fails instantly
with a typed refusal; the rest of the cluster stays commandable, and the
data plane keeps serving.

## Cloud Relationship

Ployz Cloud is a consumer and an ordinary mesh peer. It owns product
workflow state: organizations, projects, GitHub integration, build records,
billing, notifications, and durable history it chooses to keep. The cluster
owns runtime truth. Cloud writes the same rows and watches the same status
the CLI does; it does not orchestrate machine-local work.
