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
system first. Ployz has no replicated core, quorum, or consensus protocol:

- Cluster config is rows in a shared store, converged to every machine.
- Each machine's Keeper converges that machine toward the rows it does not
  own, and reports into status rows nobody else may write.
- Every machine accepts commands; followers forward cluster mutations to one
  preferred controller named by a Corrosion row.
- That controller serializes mutations with an ordinary in-memory lock and
  plans each attempt from current rows and runtime reality.
- Deploys consume prebuilt registry images; v2 does not build source code.
- Each target node uses local Duroxide and SQLite only to resume its own
  host-local prepare and retire work after a daemon crash.

The cluster should not surprise the operator. Cluster intent changes only when
a row changes; host effects remain observable runtime reality and are
re-inspected after interruption.

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
No RPC framework, no message broker. One advisory preferred controller orders
cluster mutations in the healthy case; it owns no durable workflow history.
Host workflow history stays private to the node performing the effect.

Membership is a roster row plus the mesh peer set. Machines are admitted by
SSH provisioning or a revocable multi-use join token; Ployz Cloud mints
tokens as an ordinary mesh peer. Namespace isolation is policy on the flat
mesh, never topology. Ingress and caller identity sit behind thin seams:
ingress provider (direct, Cloudflare Tunnel, Tailscale Funnel) and Principal.

## Consistency Thesis

Converged beats consensus. Coordination should stay disposable.

Every machine holds the whole cluster's config. A preferred controller reduces
ordinary races, but it is not authoritative storage: losing it permits another
machine to be appointed without restoring or migrating history. The operator
does not size, back up, or repair a replicated control service before the
cluster can be commanded again.

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

Rows normally have one writer class:

- Config rows are operator decisions, accepted through any machine's API,
  serialized by the preferred controller, and converged everywhere. Keeper
  enforces them; it never authors them.
- Status rows are machine testimony. Each machine writes only its own.
- The Controller Appointment is the named exception: any API machine passing
  the visibility brake may replace that advisory row, and LWW resolves races.
- Docker is execution reality. Status rows report it, never replace it.

Freshness is visible — mesh last-handshake age and row timestamps — and
never inferred. A command against an unreachable machine fails instantly
with a typed refusal; the rest of the cluster stays commandable, and the
data plane keeps serving.

## Cloud Relationship

Ployz Cloud is a consumer and an ordinary mesh peer. It owns product
workflow state: organizations, projects, GitHub integration, build records,
billing, notifications, and durable history it chooses to keep. The cluster
owns runtime truth. Cloud submits the same HTTP commands and watches the same
views as the CLI; it does not orchestrate machine-local work.
