# Machines Broadcast Facts; The Core Owns Intent

**Superseded by [ADR 0040](0040-corrosion-replaces-the-core-and-nats.md).**

Cluster state splits into exactly two kinds, each with one writer and its
own durable home. Neither lives in a shared store.

**Facts** are what a machine can testify about itself: its managed
containers (with content-derived identity labels), public IP, roles,
applied lifecycle, and cert material refs. Each machine publishes its whole
fact snapshot on change and unconditionally every 30 seconds, and answers a
facts request for fresh readers. The sole writer of a machine's facts is
that machine, enforced by NATS subject permissions on its minted
credential. The periodic full rebroadcast is the sync protocol: any lost
message, wiped cache, or rejoined reader heals within one tick, with no
replay, ordering, or version bookkeeping.

**Intent** is what an operator decided: the machine roster and subnets,
lifecycle (drain/resume), route bindings, serving promotions, and
authorized users. Intent's durable home is evidence files on the core
machine's disk, written only by operations
through the core's single sequencer process. The core serves intent by
request and rebroadcasts it on the same periodic drumbeat as facts; readers
re-list on reconnect and never trust having seen deltas (ADR 0005).

Everything a consumer needs is a fold over the two: the gateway table is
`intent (bindings + promoted entries) × facts (containers)`; DNS is
`intent hostnames × facts gateway addresses`; the CLI's cluster view is the
core's live fact cache plus intent, with silence rendered as silence.

Two consequences are deliberate:

- **Serving eligibility remains a commit, not an inference.** A deploy
  promotes a service entry by writing intent after successful completion —
  atomic because the core is one process. The gateway fold never serves
  from facts alone: retained failed-deploy containers are facts and must
  never receive traffic.
- **Route bindings are intent, not machine facts.** They exist
  independently of replicas, so a scale-to-zero or fully-failed service
  keeps its binding and serves the branded unavailable response (ADR 0024),
  and dead-machine drain intent survives the machine (ADR 0026).

Fencing is deliberately loose everywhere convergence makes races cheap:
deploys plan as diffs against observed reality, so retries and duplicates
converge instead of needing idempotency records, and the namespace fence is
the sequencer's in-process mutex. Strictness survives in exactly three
places, where a mistake is irreversible or externally punished: identity
minting at the membrane (names, subnets, credentials), the never-reuse
discipline for identities and entry digests (what makes loose convergence
safe), and ACME issuance (CA rate limits).

**Corelessness considered and rejected.** A fully symmetric design — every
machine an equal writer, intent as a union-merged replicated set,
operations sequenced by any machine — was designed in full and destroyed
under adversarial review, on three structural grounds: authority (a
union-merged trust set makes every machine a writable root of trust, so one
compromised node mints cluster-wide credentials and revocation races the
attacker); convergence (change-only broadcast over at-most-once transport
without anti-entropy diverges permanently, and post-heal conflicts have no
surfacing owner); and evidence (operation history scattered on sequencers'
disks is erased by routine machine removal). Each fix reintroduces a single
authority surface at higher cost than the core it replaces — the reference
implementation of the coreless philosophy ships an entire external
CRDT database to solve what this design would have hand-rolled. The
product's position between consensus and gossip is stated in
`docs/architecture/backbone.md`: no quorum to operate, and no silent
divergence either. One disposable core is that position. Its loss is
repaired by promotion plus one broadcast tick, because after this ADR the
core holds only intent files and evidence — nothing else to recover.
