# Corrosion Replaces The Core And NATS: The Control Plane Is Coreless

> Partially superseded by [ADR 0041](0041-preferred-controller-serializes-cluster-mutations.md):
> Corrosion remains the replicated store, while one disposable preferred
> controller now serializes cluster mutations in memory. Duroxide is local to
> each node and runs only that node's host prepare/retire effects. Followers
> replace the advisory appointment only after one hard connect failure, and
> public deploy operations move only from created to terminal.
>
> Identity, conflict, and join-repair details are further superseded by
> [ADR 0042](0042-canonical-names-are-resource-identities.md).

Ployz v2 removes the Control-Plane Core, the sequencer, and NATS entirely.
Cluster config is rows in a shared Corrosion store — stock, version-pinned,
multi-writer last-writer-wins, run as a plain systemd unit beside one
`ployzd` binary per machine. Transport is HTTP/JSON with SSE watches over a
pluggable WireGuard mesh, with cryptokey routing as caller identity.
Membership is a roster row plus the mesh peer set, admitted by SSH
provisioning or a revocable join token presented at the public join-only
HTTPS door every machine serves, TLS pinned by the door-cert fingerprint
carried inside the join blob; membership is write authority, so admission
is the security decision. Revocation is row deletion and converges like
any row: a partitioned door can honor a not-yet-converged revocation
until the token's TTL — the same priced stale-truth class the thesis
accepts, repaired by `machine rm`. Removal deletes the roster row, every
Keeper drops the peer, and the removed machine's writes stop propagating
with its mesh access; that fence assumes members run trusted software.
Under the single-operator trust ceiling a hostile member is the deferred
signing tier's threat model, not v1's. The door allocates each joiner's
container /24 from the operator's supernet by random-free pick with a
courtesy re-read; a collision that survives convergence is self-healed by
the lowest canonical machine name re-picking — the row law's one named exception,
on the transport subnet field. Each machine's Keeper converges that
machine's mesh substrate toward rows it does not own and reports into status
rows nobody else may write. Ordinary product rows have exactly one authority —
the operator command stream or exactly one machine — so LWW normally
adjudicates the operator racing themselves. ADR 0041 adds the explicit
multi-writer exception for advisory Controller Appointments.

The consistency thesis this rests on — converged beats coordinated, with the
LWW price stated and accepted — is `VISION.md` and
`docs/architecture/backbone.md`, rewritten when the thesis was decided.

This reverses the previous "corelessness considered and rejected" verdict
deliberately, on that design's own terms. The earlier hand-rolled
coreless design was rejected on authority, convergence, and evidence grounds,
observing
that the reference coreless implementation "ships an entire external CRDT
database to solve what this design would have hand-rolled." Shipping that
database is exactly this decision: adopting Corrosion answers convergence
with real anti-entropy instead of drumbeat rebroadcast, answers authority
with the one-writer-per-row law under a single-operator trust ceiling, and
answers evidence with coarse per-operation summary rows. Private node-local
workflow history is executor state, not a public event log. What it removes is
the entire epoch/mirror/promote recovery apparatus
a disposable core required — the recovery drill collapses to "any surviving
machine still holds everything." A three-node spike validated the bet
hands-on: sub-second propagation, tens of MiB of RSS, single-digit CPU, and
every failure drill (kill, reboot, partition, schema skew, replacement,
reseed) passed.

The design record is the
[Ployz v2 wayfinder map](https://github.com/getployz/ployz/issues/778): its
closed decision tickets hold the detail, and the `docs/design/` specs they
drafted land with the consolidated spec.

## Superseded

- **0013 (direct TLS NATS)** — no NATS; HTTP/JSON/SSE over the mesh.
- **0014 (Host Runner update separate from substrate update)** — was already
  superseded by 0037; both are replaced by one caller-paced
  `{version, sha256, url}` upgrade command with keeper-first swap.
- **0016 (the core is disposable, not replicated)** — no core to dispose of;
  availability comes from every machine holding the whole config.
- **0017 (rollout orchestration lives above the core)** — the core-shaped
  preconditions are gone; the principle survives as caller-paced upgrades.
- **0018 (machines keep a local fact ledger)** — machine testimony is rows
  the machine exclusively writes; private Duroxide/SQLite state records only
  host-local prepare and retire execution.
- **0019 (core recovery is local machine promotion)** — `core promote` is
  dead; repair is refound (teardown + fresh install + re-declared
  intent, #798) or fresh join, never promotion.
- **0020 (machine bootstrap entrypoints)** — the founder/joiner/cloud
  bootstrap split is dead; joining is one token door or SSH provisioning,
  and Cloud mints tokens as an ordinary mesh peer.
- **0026 (machine lifecycle intent is control-side durable authority)** —
  lifecycle is a field on the machine's roster row, written by the operator
  command stream and accepted by any machine.
- **0028 (machines broadcast facts; the core owns intent)** — replaced by
  the row model and the one-authority-per-row law; its corelessness
  rejection is reversed above.
- **0029 (JetStream exits: core NATS is transport, disks are storage)** —
  no NATS to classify; cluster storage is Corrosion rows, while each node's
  private SQLite database holds only its local workflow execution history.
- **0030 (hub-loss recovery: machines re-point to a promoted core)** — no
  hub, no re-pointing, no epoch.
- **0031 (recovery seams: hand-rolled epoch and mirrored intent snapshot)**
  — the epoch, the drumbeat mirror, and the candidate list are dead.
- **0033 (deploy phases promote atomically)** — phase-atomic intent
  transactions are replaced by one complete Namespace intent-row replacement:
  pre-flip failure never serves the new service map, and old generations run
  through drain.
- **0035 (fresh dataplane testimony gates new placement)** — the
  NATS-gathered testimony contract is gone; candidates answer live bids at
  the point of use, and the 275-second bound survives only as the staleness
  threshold on the mesh provider's reported last-verified age (WireGuard's
  last handshake for builtin).
- **0036 (deploy previews determine builds and receipts constrain
  placement)** — source builds, previews, and receipts are removed; current v2
  deploy accepts prebuilt registry image references.
- **0037 (Keeper reconciles one machine assignment)** — replaced by
  Keeper's charter: no assignments, components, or profiles compiled by
  Control; Keeper's converge diet is the mesh, from roster rows.
- **0038 (Keeper owns the machine network)** — the privilege split survives
  in the charter (Keeper is the sole root role), but the declared Dataplane
  Projection and the provider-neutral admission testimony contract are
  replaced by the mesh-provider seam.

## Surviving, reread in v2 terms

The product-behavior ADRs carry over with their nouns translated: 0002,
0006, 0010, 0012, and the
unamended parts of 0023 and 0024; 0003 (operations are informational records)
with deploy operations as coarse summary rows only. ADR 0041 supersedes the
old deploy planner described by 0004, 0008, 0011, and 0022. Its local advisory
admission checks do not prevent stale or partitioned commits; the next caller
retry plans from reality. 0005
(rebuild full views from invalidation) survives with Corrosion
subscriptions as the wake signal and re-query as the correctness path;
0027 (liveness surfaces at the point of use) with the mesh
provider's reported last-verified age (WireGuard's last handshake for
builtin) as the displayed evidence; 0034 (public ingress DNS is
external) with internal resolution fed by Corrosion rows instead of the
drumbeat, mirror, machine RPC, and NATS failover; 0039 (host
compatibility lives in profiles and supervisor adapters) with the typed
NATS service units becoming the v2 per-role units plus the pinned
Corrosion sidecar.
