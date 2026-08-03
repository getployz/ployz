# Scoped authority over CRDT gossip

Snapshot: **2026-08-01**. Question: how do systems scope write authority
over an unstoppable gossip/CRDT replication layer? Three tracks: what stock
Corrosion itself offers; access control in local-first/CRDT sync systems;
and the verify-at-read pattern generally, with the replay, rotation, and
key-bootstrap mechanics that make it sound. Sources: a clone of
`github.com/superfly/corrosion` (main, 2026-08), project docs, Fly.io
engineering posts, project repos and specs, and the papers cited inline.
Synthesis:
[scoped authority and the signing tier](../design/scoped-authority-and-the-signing-tier.md).

## Track 1 — Corrosion itself

Sources: repo clone of `github.com/superfly/corrosion` (main, 2026-08),
`superfly.github.io/corrosion` docs, `fly.io/blog/corrosion/`,
`fly.io/blog/skip-the-api/`.

### Transport / membership auth

- Gossip is QUIC over UDP (`gossip.addr`). Three modes in `GossipConfig`
  (`crates/corro-types/src/config.rs`): `plaintext: bool`, or `tls` with
  `cert_file`/`key_file`/`ca_file`/`insecure: bool`, plus optional mTLS via
  `tls.client { cert_file, key_file }`. Docs: "Strong encryption is highly
  recommended for any non-development usage"; plaintext is for "toy clusters
  or when the network itself handles cryptography" (i.e., exactly the
  WireGuard-mesh case).
- **mTLS is the entire authorization model**: any peer presenting a
  CA-signed cert is a full member. There is no per-table, per-row, or
  per-writer control anywhere in the codebase. No GitHub issues propose one
  (searched `auth`, `read-only`; nothing relevant).
- SWIM membership (foca) has no separate auth layer; whoever can complete
  the QUIC handshake gossips.

### HTTP API auth

- Endpoints: `POST /v1/transactions`, `/v1/queries`, `/v1/subscriptions`,
  `/v1/updates/{table}`, `/v1/migrations`, `/v1/health`, plus an optional
  Postgres wire listener (`api.pg`, which does have a `readonly` flag
  rejecting mutating statements — the only read-only knob in the product).
- Auth is one static bearer token (`api.authz.bearer-token`), enforced by a
  single axum middleware `require_authz`
  (`crates/corro-agent/src/agent/util.rs:372`) — all-or-nothing across every
  endpoint including subscriptions. `AuthzConfig` has exactly one variant:
  `BearerToken(String)`.
- The API is the *local* trust boundary (localhost sidecar). It does not
  authenticate you to the cluster; the daemon does whatever its API clients
  ask, then gossips it.

### Writer identity on changes

- Corrosion uses cr-sqlite (`doc/crdts.md`): each node's identity is the
  cr-sqlite `site_id` (random 16-byte UUID from the `crsql_site_id` table),
  called **actor_id** in Corrosion. Deleting `corrosion.db` regenerates it.
- **Every column-level change carries the writer's actor_id** and it is
  durably stored: queryable via the `crsql_changes` aggregate vtab and
  per-table `<table>__crsql_clock` tables (columns:
  `table, pk, cid, val, col_version, db_version, site_id, cl, seq`).
  `/v1/transactions` responses return `{version, actor_id}` (`ExecResponse`
  in `crates/corro-api-types/src/lib.rs`). So an application **can** read
  "which actor last wrote this column" with plain SQL through
  `/v1/queries`.
- **But actor_id is a claim, not a credential.** Incoming `ChangeV1` structs
  carry whatever actor_id the sender put in them; `process_multiple_changes`
  and the broadcast handler use it for bookkeeping (per-actor version
  tracking, clock updates) with zero cryptographic binding to the QUIC peer.
  A malicious member can spoof any other member's actor_id.

### Conflict resolution (matters for replay analysis)

- Per-column LWW, order of comparison: **biggest `col_version` wins, then
  biggest `value` (SQLite `max`, lexicographic), then `site_id`** as random
  tiebreak (`doc/crdts.md`). `col_version` is a per-column Lamport counter
  **supplied by the writer** — so any writer can permanently win any cell by
  claiming a huge `col_version`. There is no upper bound and no validation.
  This is the sharpest edge: authority scoping via reader-side signature
  checks does not stop a hostile member from making the *stored winner* be
  its row; readers must filter at read time, forever.

### Schema DDL

- Schema is **local files only** (`db.schema_paths`), applied by
  `apply_schema` at startup/reload; DDL is never gossiped. Only
  `CREATE TABLE`/`CREATE INDEX`; destructive changes (drop table/column) are
  ignored/prohibited; non-null columns need defaults; no unique indexes
  besides the PK. Consequence: a read-only member can't be given a different
  (reduced) schema per node — but schema changes are also not an attack
  channel from the mesh.

### Fly's own trust posture

- `fly.io/blog/corrosion/`: no adversarial threat model at all; safety work
  is watchdogs, regionalization ("two-level database scheme" to reduce
  "blast radius of state bugs" — operational bugs, not attackers). Members
  are mutually trusted infrastructure; "workers own their own state" is a
  *convention*, not enforced.
- `fly.io/blog/skip-the-api/`: acknowledges "shipping a database limits your
  ability to restrict access to data… you can't ship that database without
  the client seeing all the data"; the suggested mitigation is
  database-per-tenant, i.e., **partition the cluster, not the authority**.

**Bottom line:** Corrosion gives you membership-is-root, an
unauthenticated-but-durable per-column writer id, and a writer-controlled
LWW counter. Any scoped-authority scheme must live entirely in the
application layer above stock Corrosion, and must treat `col_version`,
`actor_id`, and row contents as attacker-controlled inputs.

---

## Track 2 — Access control over CRDTs / local-first sync

### Keyhive (Ink & Switch, ex-Beehive) — capability CRDT for Automerge

- Docs: [project page](https://www.inkandswitch.com/project/keyhive/),
  [notebook](https://www.inkandswitch.com/keyhive/notebook/),
  [repo](https://github.com/inkandswitch/keyhive).
- Authority: "all Automerge documents get identified by a public key, and
  delegate control over themselves to other public keys." Roles
  (pull/read/write/admin) via **chains of signed delegations** — "convergent
  capabilities": the capability graph is itself CRDT state, merging like the
  data.
- Verification: at **merge/admission time against causal position** — an op
  is valid if its author held authority at the point in causal history where
  the op sits ("ranges of authorization (and revocation) over time").
- Revocation: **coordination-free**; revocations are ops in the same graph.
  Concurrent admin-revokes-admin: both revocations apply (no winner picked).
  Backdating is acknowledged as unsolved without consensus ("a solution to
  backdating is to gain consensus… fairly counter to the local-first ethos,
  hence us avoiding it here").
- Partition: causal consistency, no consensus; revocation only takes effect
  where it has propagated.
- Maturity: **pre-alpha, explicitly "DO NOT use this release in
  production," no security audit** (as of the 2025 notebook entries).

### p2panda access control — best single survey of the space

- [Access Control in Decentralised Systems](https://p2panda.org/2025/07/28/access-control.html)
  (July 2025). Surveys capability systems (Willow/Meadowcap, UCAN, Keyhive)
  vs. **distributed ACLs as CRDTs** (localfirst/auth, their own design).
- Their design: levels `Pull/Read/Write/Manage`; only `Manage` mutates the
  group. Verification is local on every replica by DAG walk. Revocation uses
  a **"strong removal" resolver**: removal/demotion of a manager
  *retroactively invalidates that manager's concurrent operations*, mutual
  removals both proceed and both parties' concurrent ops are invalidated,
  invalidation propagates transitively. Explicitly Byzantine-aware: a
  demoted member that ignores its demotion gets its subsequent ops
  invalidated by every honest replica once the DAG merges.
- Honest caveat from the authors: strong removal "is not optimal or
  desirable for all cases" (it deletes legitimate concurrent work);
  seniority tiebreaks are an alternative.
- Open problem they name: broadcast-only systems — you cannot control who
  *receives* data, only what readers believe/decrypt. (Exactly the Corrosion
  situation.)

### OrbitDB access controllers — write ACL + per-entry identity signatures

- [ACCESS_CONTROLLERS.md](https://github.com/orbitdb/orbitdb/blob/main/docs/ACCESS_CONTROLLERS.md).
  Every oplog entry is signed by a writer identity; every replica runs
  `canAppend(entry)` at **admission/replication time**: resolve
  `entry.identity`, check it against the `write: [ids…]` list (or `'*'`),
  then `identities.verifyIdentity(...)`. Entries failing the check are not
  appended locally — the reader-refuses-to-believe pattern, at ingest.
- `IPFSAccessController`: ACL immutable (baked into the DB address —
  changing ACL = new database). `OrbitDBAccessController`: mutable
  `grant`/`revoke` stored in a companion OrbitDB store; revocation is
  non-retroactive and propagates only as that companion store replicates
  (revocation lag, no invalidation of already-admitted entries). Custom
  controllers are just `canAppend` functions.
- Maturity: shipped and stable for years, but identity model is
  pluggable/loose; no defense against a peer replaying
  admitted-then-revoked history to a fresh node that receives the old ACL
  first.

### Secure Scuttlebutt — authority by construction (single-writer logs)

- [Protocol guide](https://ssbc.github.io/scuttlebutt-protocol-guide/).
  Identity = Ed25519 keypair; each identity owns one append-only feed. Each
  message signs `{previous (hash of prior msg), author, sequence, timestamp,
  hash, content}`. Readers verify the whole chain; there is no cross-writer
  conflict because **nobody can write to anyone else's feed**. Forking your
  own feed (two messages with the same `sequence`) is detectable and
  effectively bricks the feed. Replay of old messages is a no-op: readers
  already have sequence N. Revocation/rotation: essentially none (identity
  loss is permanent) — the known weak point.

### Hypercore / Autobase — signed Merkle log, fork counters

- [docs.pears.com/building-blocks/hypercore](https://docs.pears.com/building-blocks/hypercore).
  Single Ed25519 writer signs the **Merkle tree root over
  (key, treeHash, length, fork)** — one signature authenticates the entire
  prefix; readers verify block proofs against the signed root. The **`fork`
  counter increments on truncation**, which is their explicit
  rollback/replay defense: a stale or rewound view is distinguishable
  because it carries an older (length, fork). Multi-writer is layered above
  via Autobase (ordering multiple single-writer cores) — same "authority by
  construction" as SSB, with better rotation ergonomics.

### Iroh (iroh-docs) — dual-signature entries, closest shape to "signed rows"

- [docs.iroh.computer/protocols/kv-crdts](https://docs.iroh.computer/protocols/kv-crdts),
  [github.com/n0-computer/iroh-docs](https://github.com/n0-computer/iroh-docs).
  A replica is a KV store under a **NamespaceId** — "the public key of a
  keypair that gates write access." Entries are identified by
  `(namespace, author, key)`; entry value = BLAKE3 hash of content + size +
  **timestamp**; "All entries in a replica are signed with two keypairs: the
  _Namespace_ key, as a token of write capability, and the _Author_ key, as
  a proof of authorship." Conflict resolution is timestamp LWW per key.
  Readers verify both signatures at sync admission.
- Failure modes they inherit: possession of the namespace secret =
  unrevocable cluster-wide write (no delegation granularity, no rotation
  story); timestamp LWW means any writer can post `timestamp = MAX` and own
  a key forever — same writer-controlled-counter hole as Corrosion's
  `col_version`. Status: split out of iroh core, community/n0-maintained,
  not the flagship path.

### Matrix state resolution — auth rules evaluated at merge time

- [spec.matrix.org/v1.11/rooms/v11](https://spec.matrix.org/v1.11/rooms/v11/).
  Every event carries `auth_events` (pointers to the state that authorized
  it); events are signed by the sender's server. On receipt, servers run
  authorization rules against that auth chain; unauthorized events are
  **rejected but retained**. On divergent histories, **state resolution
  v2**: pick out "power events" (power levels, join rules, bans/kicks —
  "events that might remove someone's ability to do something"), order them
  (sender power desc, then timestamp), then **iteratively re-run auth
  checks**, so demotions are applied before the concurrent ops they should
  have blocked — a user demoted in partition A cannot smuggle ops through
  partition B; on merge the demotion wins retroactively. This is the most
  battle-tested "re-evaluate authority at merge" design in production
  (federated, actively attacked). Cost: state resolution is notoriously
  subtle; v1 had real exploited resurrection bugs (the "state reset" era),
  which is *why* v2 exists.

---

## Track 3 — the verify-at-read pattern

### SUNDR — fork consistency (untrusted store, signing clients)

- [Li, Krohn, Mazières, Shasha, OSDI '04](https://www.usenix.org/legacy/event/osdi04/tech/full_papers/li_j/li_j.pdf).
  Clients sign **version structures** (version vectors + Merkle roots of the
  filesystem), the server just stores. An untrusted server cannot forge
  writes; the best it can do is **fork** clients into disjoint views — and
  "if the server delays just one user from seeing even a single change by
  another, the two users will never again see one another's changes," which
  is detectable via any out-of-band exchange. Canonical result: with
  reader-side verification and no trusted sequencer, **fork/rollback is the
  residual attack**, and the countermeasure is cross-checking recent signed
  state between honest parties — gossip is actually the cure here, and an
  honest gossip layer among non-malicious members already exists in a mesh.

### TUF — the reference design for replay/rollback/freeze and key rotation

- [Spec](https://theupdateframework.github.io/specification/latest/).
  Directly answers "can an attacker re-inject a victim's OLD signed row?" —
  TUF's named attacks: rollback (serving older-signed data), freeze (serving
  current data forever), fast-forward, mix-and-match. Mitigations, all
  reader-side:
  - **Monotonic version numbers inside the signed payload**; clients persist
    the last-seen version and refuse regressions.
  - **Expiration timestamps inside the signed payload**; a frequently
    re-signed short-lived **timestamp role** bounds freshness, so a replayed
    old-but-validly-signed object dies at its expiry.
  - **Snapshot role** signs the version of every other metadata file,
    preventing mix-and-match of individually-valid pieces.
  - **Key rotation**: root signs the key sets of all other roles; a new root
    must be signed by a **threshold of both the predecessor's keys and its
    own**, and clients walk the root chain forward — old data stays
    verifiable because the chain of custody is itself signed. This is the
    standard answer to "how does a reader learn/update the key set from a
    bootstrap trust root."

### SPKI/SDSI — authorization bound to keys, not names

- [RFC 2693](https://datatracker.ietf.org/doc/html/rfc2693). Authorization
  certificates grant a *capability* directly to a public key, with
  delegation chains and 5-tuple reduction at verification time. Ancestor of
  UCAN/Keyhive/Meadowcap. Relevant lesson: put the *scope* (which
  tables/rows/columns a key may write) in the signed grant, and let readers
  do chain reduction; no online authority needed at verify time. Its known
  weakness is the same as every offline-capability system: revocation
  requires either short validity windows or online revalidation.

### Certificate Transparency / key transparency — gossip as the fork detector

- [RFC 6962](https://datatracker.ietf.org/doc/html/rfc6962), CONIKS (USENIX
  Sec '15). Append-only Merkle logs with signed tree heads; the residual
  attack is again the **split view**, countered by gossiping STHs among
  monitors/auditors. Same shape as SUNDR: signatures give
  integrity/authority; **freshness and non-equivocation require comparing
  signed heads across parties**.

### Kleppmann, "Making CRDTs Byzantine Fault Tolerant" (PaPoC '22)

- [PDF](https://martin.kleppmann.com/papers/bft-crdt-papoc22.pdf). Claims
  and mechanics:
  - Tolerates **any number** of Byzantine nodes (Sybil-immune) while keeping
    Strong Eventual Consistency, with "only modest changes to existing CRDT
    algorithms."
  - Mechanism: every update is identified by its **hash**; each update
    embeds **predecessor hashes** (causal deps), forming a hash-DAG
    "resembling a Git commit history." Nodes only deliver an update after
    its predecessors; heads-exchange makes sync integrity-checked ("if two
    nodes exchange the hashes of their current heads and find them
    identical, the set of updates they have observed is also identical").
  - **Unique IDs = hash of the update**, killing the (replica-id, counter)
    forgery/duplication attacks — directly the fix for cr-sqlite-style
    writer-supplied `col_version`/`site_id`.
  - Equivocation and fake dependencies are absorbed: "Byzantine nodes may
    add arbitrary vertices and edges… it is not possible for Byzantine nodes
    to do anything that would prevent correct nodes from delivering the same
    set of updates." An update referencing a nonexistent hash simply never
    gets delivered.
  - Note: Corrosion's wire format has none of this (no hashes, no
    signatures, writer-asserted causality), so this is a pattern for the app
    layer, not something to retrofit into the sidecar.

### Replay under LWW, distilled

Across systems, three ways a victim's old-but-genuinely-signed row is
prevented from reverting state:

1. **Sign the logical clock into the payload and keep reader-side
   high-water marks** (TUF versions, SSB sequence, Hypercore length+fork):
   a reader that has seen `(writer, key, n)` refuses `(writer, key, m<n)`.
   Requires readers to persist per-writer/per-key watermarks.
2. **Bind the row to causal context** (Kleppmann hash-DAG, Matrix
   auth_events): the old row's signature covers its predecessors, so
   re-injecting it doesn't place it "after" current state — it's just an
   already-merged ancestor.
3. **Expiry / freshness attestation** (TUF timestamp role, CT STH gossip):
   stale signed state dies of old age; a live signer periodically re-signs a
   head. Handles freeze, not just rollback.

Under Corrosion's LWW specifically, the app-layer signature can't stop the
*store* from converging to the attacker's row (the attacker wins
`col_version`), so the pattern must be: signature covers
`(table, pk, column values, writer's own monotonic seq)`; readers verify +
enforce scope + enforce watermark, and treat "current stored value fails
verification" as "row absent / use last verified value from an app-kept
shadow" — which in turn means readers need somewhere durable to remember the
last *verified* state, or the attacker's overwrite becomes a deletion
attack. That is the honest ceiling of verify-at-read: **an unstoppable
writer can always vandalize; it just can't impersonate**.

---

## Synthesis — recurring patterns for scoping write authority over an unstoppable gossip layer

**Pattern A: Authority by construction — partition the keyspace per writer**
(SSB, Hypercore, Iroh authors, Fly's "workers own their own state"
convention, "database per tenant"). Each writer only ever writes rows whose
identity embeds the writer; readers believe a row only if
`row.owner == signer`. In Corrosion this maps cleanly: per-machine rows
signed by a machine-held key. Failure modes: nothing protects *shared* rows
(cluster-wide config still needs Pattern B/C); replay of the writer's own
old row needs a signed monotonic counter + reader watermark; key loss = that
writer's namespace is attacker-writable until rotation lands cluster-wide.
Maturity: highest — the only pattern with a decade+ of production use.

**Pattern B: Signed rows + reader-side ACL check ("verify at
read/admission")** — OrbitDB canAppend, Iroh dual signatures, and the
operator-signed-rows idea. Signature must cover
`(table, pk, column values, writer id, writer's own monotonic sequence)` —
signing the value alone permits both replay and cross-row transplantation of
a signed cell. Readers hold a trust root (operator key) that signs
per-writer scope grants (SPKI-style: "key K may write table T rows where
owner=K"), verify chains offline, and *skip* rows that fail. Failure modes:
(1) replay of old signed state under LWW — mitigated only by
sequence-in-signature plus persistent reader watermarks, or TUF-style expiry
on grants; (2) revocation lag — a revoked writer is believed until the
revocation row propagates, and a partitioned reader may never see it: use
short-lived grants (revocation = stop renewing) rather than revocation
records; (3) the store still converges to garbage — verified reads need a
shadow of last-good state or the attack degrades to deletion; (4) key
rotation needs a TUF-style signed chain from the trust root so old rows stay
verifiable.

**Pattern C: Authority evaluated at merge against causal position** (Matrix
state res v2, Keyhive, p2panda strong removal). The ACL is itself replicated
data; on merge, authority-changing ops are ordered first and everything is
re-validated, so demotions beat concurrent writes retroactively. Failure
modes: backdating (a Byzantine writer forges causal position to before its
revocation — Keyhive names this unsolved without consensus; Matrix counters
it only because servers sign and cross-check event DAGs); enormous
implementation subtlety (Matrix v1 state resets were exploited in
production); Keyhive/p2panda are pre-alpha/research. Corrosion's LWW column
model has no causal DAG to evaluate against, so this pattern requires
building a Kleppmann-style hash-DAG in application tables — heavy.

**Pattern D: Fork/freshness detection via cross-signed heads** (SUNDR, CT,
TUF timestamp role). Signatures never give freshness; every mature system
adds a periodically re-signed head (heartbeat row signed with current
sequence + wall time) and lets readers cross-check heads from multiple
parties. On a gossip mesh this is nearly free: honest members already
exchange everything, so a reader that sees writer W's head at seq 100 from
one path and an attacker replaying seq 90 elsewhere detects the rollback.
Failure modes: an isolated reader (eclipse/partition) can be frozen for as
long as the head validity window; window length is the
availability-vs-staleness dial; needs a modest amount of reader-persisted
state.

**Cross-cutting Corrosion-specific constraints:** actor_id is spoofable and
`col_version`/timestamps are writer-controlled, so *nothing inside stock
Corrosion can anchor any of these patterns* — the anchor must be app-layer
Ed25519 keys, signatures stored in row documents, verification in every
reader (the ployz daemon), and the operator key as the TUF-style root that
signs writer-scope grants with expiry. Read-only members fall out for free:
a member with no grant can inject rows all day; no reader ever believes
them.

**Source index:** [corrosion repo](https://github.com/superfly/corrosion)
(`doc/crdts.md`, `crates/corro-types/src/config.rs`,
`crates/corro-agent/src/agent/util.rs`,
`crates/corro-api-types/src/lib.rs`) ·
[gossip config](https://superfly.github.io/corrosion/config/gossip.html) ·
[api config](https://superfly.github.io/corrosion/config/api.html) ·
[schema](https://superfly.github.io/corrosion/schema.html) ·
[api](https://superfly.github.io/corrosion/api/index.html) ·
[fly.io/blog/corrosion](https://fly.io/blog/corrosion/) ·
[fly.io/blog/skip-the-api](https://fly.io/blog/skip-the-api/) ·
[Keyhive notebook](https://www.inkandswitch.com/keyhive/notebook/) ·
[keyhive repo](https://github.com/inkandswitch/keyhive) ·
[p2panda access control](https://p2panda.org/2025/07/28/access-control.html) ·
[OrbitDB access controllers](https://github.com/orbitdb/orbitdb/blob/main/docs/ACCESS_CONTROLLERS.md) ·
[SSB protocol guide](https://ssbc.github.io/scuttlebutt-protocol-guide/) ·
[Hypercore](https://docs.pears.com/building-blocks/hypercore) ·
[iroh-docs](https://github.com/n0-computer/iroh-docs) /
[kv-crdts](https://docs.iroh.computer/protocols/kv-crdts) ·
[Matrix rooms v11](https://spec.matrix.org/v1.11/rooms/v11/) ·
[SUNDR OSDI'04](https://www.usenix.org/legacy/event/osdi04/tech/full_papers/li_j/li_j.pdf) ·
[TUF spec](https://theupdateframework.github.io/specification/latest/) ·
[RFC 2693 SPKI](https://datatracker.ietf.org/doc/html/rfc2693) ·
[RFC 6962 CT](https://datatracker.ietf.org/doc/html/rfc6962) ·
[Kleppmann BFT-CRDTs PaPoC'22](https://martin.kleppmann.com/papers/bft-crdt-papoc22.pdf)
