# Corrosion Replaces The Core And NATS: The Control Plane Is Coreless

Ployz v2 removes the Control-Plane Core, the sequencer, and NATS entirely.
Cluster config is rows in a shared Corrosion store — stock, version-pinned,
multi-writer last-writer-wins, run as a plain systemd unit beside one
`ployzd` binary per machine. Transport is HTTP/JSON with SSE watches over a
pluggable WireGuard mesh, with cryptokey routing as caller identity.
Membership is a roster row plus the mesh peer set, admitted by SSH
provisioning or a revocable join token; membership is write authority, so
admission is the security decision. Each machine's Keeper converges that
machine's mesh substrate toward rows it does not own and reports into status
rows nobody else may write. Every row has exactly one authority — the
operator command stream or exactly one machine — so LWW only ever
adjudicates the operator racing themselves.

The consistency thesis this rests on — converged beats coordinated, with the
LWW price stated and accepted — is `VISION.md` and
`docs/architecture/backbone.md`, rewritten when the thesis was decided.

This reverses ADR 0028's "corelessness considered and rejected" verdict
deliberately, on that ADR's own terms. 0028 rejected a *hand-rolled*
coreless design on authority, convergence, and evidence grounds, observing
that the reference coreless implementation "ships an entire external CRDT
database to solve what this design would have hand-rolled." Shipping that
database is exactly this decision: adopting Corrosion answers convergence
with real anti-entropy instead of drumbeat rebroadcast, answers authority
with the one-writer-per-row law under a single-operator trust ceiling, and
answers evidence with per-operation summary rows plus driver-local detail
logs. What it removes is the entire epoch/mirror/promote recovery apparatus
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
  the machine exclusively writes; machine-local detail is per-operation
  JSONL evidence, not a SQLite commit point.
- **0019 (core recovery is local machine promotion)** — `core promote` is
  dead; repair is reseed or fresh join, never promotion.
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
  no NATS to classify; storage is Corrosion rows plus machine-local
  evidence files.
- **0030 (hub-loss recovery: machines re-point to a promoted core)** — no
  hub, no re-pointing, no epoch.
- **0031 (recovery seams: hand-rolled epoch and mirrored intent snapshot)**
  — the epoch, the drumbeat mirror, and the candidate list are dead.
- **0035 (fresh dataplane testimony gates new placement)** — the
  NATS-gathered testimony contract is gone; candidates answer live bids at
  the point of use, and the 275-second handshake bound survives only as the
  staleness threshold.
- **0036 (deploy previews determine builds and receipts constrain
  placement)** — build is its own caller-composed operation against a
  bid-chosen builder serving an OCI facade; no preview, no receipts.
- **0037 (Keeper reconciles one machine assignment)** — replaced by
  Keeper's charter: no assignments, components, or profiles compiled by
  Control; Keeper's converge diet is the mesh, from roster rows.
- **0038 (Keeper owns the machine network)** — the privilege split survives
  in the charter (Keeper is the sole root role), but the declared Dataplane
  Projection and the provider-neutral admission testimony contract are
  replaced by the mesh-provider seam.

## Surviving, reread in v2 terms

The product-behavior ADRs carry over with their nouns translated: 0002,
0004, 0007, 0008, 0010, 0011, 0012, 0022, 0023, 0024, 0025, 0032, 0033,
0034, and 0039 as written; 0003 (operations are informational records) with
operations as summary rows plus driver-local detail; 0005 (rebuild full
views from invalidation) with Corrosion subscriptions as the wake signal
and re-query as the correctness path; 0027 (liveness surfaces at the point
of use) with WireGuard last-handshake age as the displayed evidence.
