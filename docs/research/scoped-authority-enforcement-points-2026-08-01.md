# Scoped authority: enforcement points in the v2 design

Snapshot: **2026-08-01**. Question: where can authority be enforced in the
coreless v2 design, which write paths bypass enforcement entirely, and what
do the drafted schema and specs already reserve for the deferred signing
tier? Sources: the v2 design specs under `docs/design/`, `VISION.md`,
`docs/architecture/backbone.md`, ADR 0040, and the research corpus, as they
read at this snapshot. Synthesis:
[scoped authority and the signing tier](../design/scoped-authority-and-the-signing-tier.md).

## 1. Write paths

**Who runs Corrosion: machines only.** The per-machine systemd unit list is
the only place a `corrosion.service` exists — "base (every machine): …
corrosion.service unpriv stock pinned binary"
(`docs/design/binary-crate-topology.md` § The process map). Peers do not run
Corrosion agents anywhere in the design:

- CLI: "The CLI is a remote mesh peer, not a cluster node. It connects over
  WireGuard … and reaches one cluster machine's HTTP/JSON endpoint"
  (`docs/design/cli-token-status-doctor-ux.md`, "Two facts", fact 1). Its
  dial is "userspace WG, no root (gotatun + smoltcp + hyper connector)"
  (`docs/design/mesh-provider-and-principal.md` § Provider matrix) — an HTTP
  client, no gossip stack.
- Cloud: "Cloud writes the same rows and watches the same status the CLI
  does" (`VISION.md` § Cloud Relationship); the SDK transport is "a thin
  hand-written client — `fetch()` for request/reply, `EventSource` for SSE"
  (`docs/design/binary-crate-topology.md` § SDK generation and transport).
  The phrase "Cloud subscribes as a mesh peer" (same file, § Cross-version
  compatibility, rule 1) means SSE watches over the mesh, not gossip
  membership — the transport section is explicit.

**But peers can *reach* machines' Corrosion ports.** The only fence on the
Corrosion port is roster membership, not machine-hood: "Corrosion gossip
rides inside WireGuard and cryptokey routing only admits roster peers, so a
live foreign gossiper cannot reach your Corrosion port"
(`cli-token-status-doctor-ux.md` § Finding 3). Roster = machines ∪ peers
(`mesh-provider-and-principal.md` § Identity). Corrosion is configured to
"bind ULA" (`docs/design/ployz-init-machine-one.md` § The mint list, step
6), and the ULA control plane "carries: Corrosion gossip, HTTP API, SSE"
(`mesh-provider-and-principal.md` § Builtin addressing). No design doc
firewalls the Corrosion gossip/API port down to machine addresses. See §5
for the consequence.

**Write paths, exhaustively:**

| Path | Route | Process that could enforce authorization |
|---|---|---|
| Operator CLI | laptop (peers row, userspace WG) → any machine's api fold HTTP/JSON → that machine's local Corrosion → gossip | the answering machine's **api fold** (`ployzd-api.service`), at Principal resolution |
| Cloud | identical: ordinary mesh peer → api fold HTTP via TS SDK (`VISION.md` § Cloud Relationship; `binary-crate-topology.md` § SDK) | the answering machine's **api fold** |
| Join door | joiner presents token at the public join-only HTTPS endpoint; the **admitting machine's door handler** writes the machines/peers row + allocates the /24 (`mesh-provider-and-principal.md` § Join; § IPv4 /24 allocation) | the **door handler** (token verification; `Principal::ApiToken`) |
| Machine testimony | each machine's own folds write `containers`, `machine_status`, `operations`, `cert_holdings`, `acme_http01` into local Corrosion (`corrosion-row-model.md` § Ownership map) | **nobody** — a local process writing its local agent; the row-ownership law is convention + keying, not enforced by any process |
| Keeper | never authors rows except (a) status testimony ("reports into status rows nobody else may write", `VISION.md` § Product Bet) and (b) the one named exception: rewriting its **own** machines-row `transport.subnet_v4` on duplicate-subnet loss (`mesh-provider-and-principal.md` § IPv4 /24 allocation and self-heal) | nobody — local write to local agent |
| `ployz init` | machine one writes initial `cluster` + `machines` rows "through the live api" (`ployz-init-machine-one.md` mint list step 8), enrolls the driver's peers row (step 10) | local api fold (trivially trusted — it's the founding host) |
| **Corrosion gossip itself** | any machine's Corrosion agent syncs any row to any other agent, multi-writer LWW, no per-row auth in stock Corrosion | **nobody**. This is the trust ceiling: "Membership is write authority" (`backbone.md` § Trust Ceiling) |

Key structural fact: **the api fold is the only authorization-capable choke
point, and it only covers the CLI/Cloud/join paths.** Machine-originated
writes and gossip replication have no enforcement process at all; the
row-ownership law is honored by trusted software ("that fence assumes
members run trusted software", ADR 0040 ¶1).

## 2. Principal resolution

- **Where:** "Address→identity mapping is deliberately *not* a method: it is
  one uniform rule … living once in the HTTP layer"
  (`mesh-provider-and-principal.md` § The seam). I.e., resolution lives once
  in the api fold's HTTP layer: source address → roster lookup (machines ∪
  peers) → `Principal`.
- **Per-variant authorization:** "Resolution produces a Principal; handlers
  authorize against the variant and never touch addresses or keys
  themselves" (§ Identity). So authorization is per-handler, against the
  enum variant.
- **Endpoint→authority mapping already specified:**
  - `ApiToken(TokenId)`: "honored at exactly one endpoint: join" — stated
    twice (enum doc comment in § Identity, and § Join).
  - `Machine` vs `Peer`: "distinct because their authority differs: machines
    write testimony, peers issue commands" (§ Identity). That is the entire
    specified authority matrix — coarse variant-level, no per-endpoint table
    beyond the ApiToken/join pin.
  - "Laptop and Cloud share `Peer` — single-operator trust makes them the
    same authority" (§ Identity) — i.e., v1 explicitly declines to
    distinguish peer authorities. Scoping supersedes this stated
    equivalence.
  - Rejection rule: "Any transport that cannot arrive as a Principal variant
    plus a `Transport` union variant on a roster row is rejected as a second
    system. There is no side door, no ambient identity" (§ Identity).
  - "future variants additive" comment on the Principal enum — the reserved
    hook for new authority classes.

## 3. Retrofit hooks

**The DDL reserves NO writer, timestamp, or signature fields.** Full column
inventory of `docs/design/corrosion-schema-v1.sql`: every table is
`id TEXT PRIMARY KEY, document TEXT` plus virtual generated columns.
Generated columns, exhaustively: `machines(name, lifecycle)`,
`tokens(kind)`, `namespaces(name)`, `services(namespace_id, name)`,
`route_bindings(hostname, service_id, namespace_id)`,
`containers(machine_id, service_id, namespace_id)`,
`operations(kind, state, machine_id)`,
`cert_holdings(hostname, machine_id, expires_at)`,
`acme_http01(machine_id)`. `cluster`, `machine_status`, `peers` (the latter
defined in `mesh-provider-and-principal.md` § The peers table) have no
generated columns beyond PK/document (+`peers.name`). No `writer`, no
`signed_by`, no `sig`, no `written_at` anywhere.

What "per-row writer identity" actually is in the current design:

- **Machine-authority rows only**: identity via keying, not a field —
  "Machine-authority rows are keyed so that no two machines can ever address
  the same row" (`corrosion-row-model.md` § The row-ownership law).
  Concretely: `containers.machine_id` (document field), `machine_status` PK
  = machine ULID, `operations.machine_id`, `cert_holdings` PK =
  `<machine-ulid>:<hostname>`, `acme_http01.machine_id`.
- **Operator-authority rows** (`cluster`, `machines`, `tokens`,
  `namespaces`, `services`, `route_bindings`, `peers`) carry **no writer
  identity at all** in the draft schema. VISION's "Rows carry writer and
  timestamp so a fold can be surfaced after the fact" (`VISION.md` §
  Consistency Thesis) and backbone's "Rows carry writer identity and
  timestamp" (`backbone.md` § Row Rules) are **not implemented by any DDL or
  document field in the drafts** — a real spec/schema gap. (Corrosion
  internally tracks actor-id/db-version bookkeeping, but no design doc
  claims or relies on it.)

What "the retrofit stays open" concretely rests on, per `backbone.md` §
Trust Ceiling: "The retrofit stays open by construction: per-row writer
identity, additive schema, reseed." I.e., three mechanisms:

1. **Additive evolution**: "SQL DDL is additive-only forever (Corrosion
   refuses destructive changes); document shape evolves inside the JSON,
   governed by the `v` field" (`corrosion-schema-v1.sql` header); "`v`
   integer in every document, skip-if-newer … unknown fields tolerated" +
   rollout-ordering law (`corrosion-row-model.md` § Cross-cutting
   conventions). A `sig`/`signer` field can be added to documents without
   DDL change; a `v` bump makes unsigned-unaware readers skip (fail-closed).
2. **Writer-scoped keying** on machine testimony (above).
3. **Reseed** as the compaction/escape hatch ("full-cluster reseed is the
   escape hatch and upgrade path", `backbone.md` § Row Rules).

## 4. Join/token machinery

- **Format:** issued string `pz_<token-ulid>.<32-byte-random-base64>`, shown
  once; row keeps `sha256(secret part)`; verification is O(1) lookup by
  embedded ULID + constant-time compare; plain sha256 deliberate (256-bit
  random secrets, not passwords) (`corrosion-row-model.md` § No secret
  values; `corrosion-schema-v1.sql` tokens comment).
- **UX blob:** `pzjoin_<base64 of secret + cluster door-cert fingerprint +
  member endpoints>` — one thing to copy (`cli-token-status-doctor-ux.md` §
  token create).
- **The door:** every machine serves one public join-only HTTPS endpoint;
  TLS = one cluster door keypair minted at init, pinned by fingerprint,
  "handed to each joiner inside the join response, machine-local at rest,
  never a row" (`mesh-provider-and-principal.md` § Join). Note for
  least-privilege: the doc does not distinguish machine joiners from peer
  joiners here — if roaming peers also receive the door private key, a
  scoped peer can impersonate the join door. Ambiguity worth pinning.
- **Kind lives in the request, not the token:** "The joiner declares machine
  vs roaming-peer at the door; the token carries no kind"
  (`cli-token-status-doctor-ux.md` § ployz token). Yet **the DDL already
  reserves `tokens.kind`**:
  `kind TEXT GENERATED ALWAYS AS (json_extract(document, '$.kind')) VIRTUAL`
  + `CREATE INDEX tokens_kind` (`corrosion-schema-v1.sql`) — an existing,
  indexed, currently-unused additive hook for scoped/typed credentials.
- **Tokens authority:** "operator/Cloud" (`corrosion-row-model.md` §
  Ownership map); revoke = delete row = invalidation (O(1) lookup fails);
  expiry checked at point of use; no use-count write-back (would be a
  machine writing an operator-authority row — forbidden)
  (`cli-token-status-doctor-ux.md` § token revoke, § token list).
- **Peers table:** `id, document, name(virtual)`; document carries the same
  `Transport` union, no `subnet_v4`; operator authority; swept by `peer rm`
  (`mesh-provider-and-principal.md` § The peers table). A `scope`/`role`
  field is a pure document addition — tolerated-unknown-fields makes it
  schema-legal today; a generated column + index can be added later (DDL
  additions are the one allowed DDL change class).
- **Deferred tier named:** "the broader signed-writes / API-token tier is
  deferred entirely, per #782" (`cli-token-status-doctor-ux.md` § ployz
  token). So issue #782 is the parent for both API tokens and signed writes.
- **Enrollment without the door:** init enrolls the driver (laptop or Cloud)
  as the first peers row over its own channel — ssh or `--cloud-token`
  carrying Cloud's pubkey (`ployz-init-machine-one.md` § Two drivers; mint
  step 10). A scope field would need to ride this path too, not just the
  token door.

## 5. Cheapest peer scoping — sketch + stress

**Minimal additive design consistent with the laws:**

- Add `scope` to the `peers` document (e.g.
  `"scope": "admin" | "read" | {"plugin": [...]}`), defaulting absent→admin
  for grandfathered rows. Optionally mirror on `tokens` documents so a token
  pre-binds the scope the door stamps into the peers row at admission (the
  door writing an operator-authority row while executing the operator's
  token-backed command is already the sanctioned pattern —
  `corrosion-row-model.md` § row-ownership law: "a machine writes only while
  executing the operator's explicit command").
- Extend `Principal::Peer(PeerId)` to carry (or be resolved alongside) the
  scope read from the peers row at Principal-resolution time — the "one
  uniform rule … living once in the HTTP layer" already does the roster
  lookup, so scope arrives for free.
- Handlers already "authorize against the variant" — scoped authorization is
  the same match, one level deeper. Enforcement at every machine's api fold.
- Ship enforcement binaries everywhere first (rollout-ordering law), then
  mint scoped peers.

**Stress — what leaks:**

1. **The gossip-port bypass (the big one).** Peers do not run Corrosion, but
   they can *reach* every machine's Corrosion port: cryptokey routing admits
   "roster peers" — machines ∪ peers — and that reachability is the only
   stated fence (`cli-token-status-doctor-ux.md` § Finding 3;
   `ployz-init-machine-one.md` step 6 "corrosion (bind ULA)"). Stock
   Corrosion has no gossip authn of its own; a "read-only" peer that speaks
   the sync protocol (or hits Corrosion's own HTTP API if it binds the ULA —
   unspecified which ports bind where) writes any row, bypassing the api
   fold entirely. **API-door enforcement is bypassable today.** Required
   closer: Keeper (which already owns "WG/eBPF/sysctls/firewall",
   `binary-crate-topology.md` § process map, and already converges per-peer
   WG state from roster rows) converges a firewall rule: Corrosion port
   accepts only machine ULAs, never peer ULAs. Note machine and peer /112s
   share the same derived /48 with no structural prefix distinction
   (`mesh-provider-and-principal.md` § Builtin addressing; § peers table),
   so the rule must be roster-driven per-address, not prefix-based. Without
   this, no peer scoping is sound.
2. **SSE/read scoping.** A read-only scope still sees the *entire* cluster
   config — the no-secrets law makes that survivable by design ("A Corrosion
   dump must be shareable with support", `corrosion-row-model.md` § No
   secret values), **except** env fingerprints: "Unsalted sha256 of a
   low-entropy value is dictionary-guessable by mesh members — priced in;
   membership is the trust ceiling" (same section; reaffirmed in ADR 0040's
   0022 rereading). A least-privilege plugin peer inherits dictionary access
   to every service's env fingerprints unless reads are row-filtered, which
   the flat replicated store makes expensive — but once the gossip port is
   fenced, all peer reads ride the api fold, where filtering is cheap. Also:
   operation detail is driver-local JSONL streamed over SSE
   (`corrosion-row-model.md` § Admission lens) — a second read surface to
   gate.
3. **Partitioned revocation of a downgrade.** Scope changes converge like
   any row; ADR 0040 already prices the analog: "a partitioned door can
   honor a not-yet-converged revocation until the token's TTL" (¶1). But a
   peers-row scope has **no TTL bound** — a machine partitioned from the
   downgrade write honors the old scope indefinitely until convergence. Same
   accepted stale-truth class, but unbounded; hard revocation remains
   `peer rm` + Keeper dropping the WG peer (which is also
   convergence-dependent per-machine).
4. **LWW race on the scope field — one authority holds.** Peers and tokens
   rows are operator-authority (`corrosion-row-model.md` § Ownership map;
   `mesh-provider-and-principal.md` § peers table). Scope mutation stays in
   the operator command stream, so LWW only adjudicates the operator racing
   themselves — inside the accepted price. One caveat: admission-time
   stamping (door) racing an operator's concurrent scope edit is
   machine-executing-operator-command vs operator — nominally still "the
   operator's command stream" on both sides, but it is the closest the
   design comes to two writers on one row; keep the door's write
   insert-only.
5. **Mixed-version enforcement.** Old binaries ignore unknown fields — an
   unscoped-aware machine treats a scoped peer as full `Peer` authority
   (silent escalation). The `v` discipline is the fix and it is fail-closed:
   bump `v` on scoped peers rows → old readers "skip the row and report it"
   (`corrosion-row-model.md` § Cross-cutting conventions) → Keeper on old
   machines never converges that WG peer → the scoped peer cannot even reach
   old machines. Cost: the peer is dark to lagging machines; `doctor`
   Finding 2 already surfaces skipped newer-`v` rows cluster-wide
   (`cli-token-status-doctor-ux.md` § Finding 2).
6. **Doctor/status surfacing.** `status` shows machines only
   (NAME/ROLE/HANDSHAKE table); no `peer ls` is specced anywhere;
   `token list` shows no kind/scope. A scope tier needs: a peer listing
   surface, scope in `token list`, and a doctor finding for "peer scoped
   down but machine X hasn't converged the row" — none exist yet. Doctor's
   copy-paste-repair pattern is the natural home.
7. **Door key distribution** (from §4): if peer joiners receive the cluster
   door private key in the join response, every scoped peer can impersonate
   the fingerprint-pinned door. Must be restricted to machine joiners.

**Hostile-edge machines are a different problem entirely:** a machine runs a
Corrosion agent, and gossip is unauthenticated multi-writer — a hostile
machine writes/deletes *any* row (roster included) regardless of api-fold
scoping. ADR 0040 concedes this: "that fence assumes members run trusted
software. Under the single-operator trust ceiling a hostile member is the
deferred signing tier's threat model, not v1's." Operator-signed rows
authenticate row *content* additively (sig field in document + `v` bump),
but signing cannot stop a hostile gossip member from tombstoning rows or
replaying old signed rows — deletion and presence have no signature to
check. The signing tier needs an answer for deletes/replay that no current
doc sketches.

## 6. Other bearing material

- **Trust ceiling, canonical statements:** `VISION.md` § Consistency Thesis
  ("membership is write authority … admission is the security decision.
  Operator-signed rows are deferred … the additive schema and per-row writer
  identity keep that retrofit open"); `backbone.md` § Trust Ceiling (adds
  "return as their own effort" and the three-mechanism "by construction"
  list); ADR 0040 ¶1 (adds the partition-revocation price and the
  trusted-software assumption on the removal fence).
- **Backbone explicitly rejects building it early:** "Signing, versioning,
  or ordering machinery ahead of real demand" (`backbone.md` § What This
  Rejects). Any scoping proposal must argue real demand or thin-seam status.
- **Removal fence:** "Removal deletes the roster row, every Keeper drops the
  peer, and the removed machine's writes stop propagating with its mesh
  access" (ADR 0040 ¶1) — i.e., write-revocation for machines = mesh
  eviction, enforced by every Keeper independently, convergence-paced.
- **`cluster_id` fencing is data hygiene, not authorization:** "inert in v1;
  the cells seed and the reseed/stray-node data fence. Readers drop
  foreign-`cluster_id` rows" (`corrosion-row-model.md` § Cross-cutting
  conventions); doctor Finding 3 treats foreign rows as contamination, and
  its reasoning (*"a live foreign gossiper cannot reach your Corrosion
  port"*) is exactly the sentence documenting that mesh reachability is the
  sole gossip fence.
- **The no-quorum law bounds solutions:** "No quorum locks exist anywhere in
  the product" (`corrosion-row-model.md` § Uniqueness without a
  coordinator); scoping/signing designs cannot use coordination (no
  revocation quorum; converged revocation only).
- **The Peer=Cloud=laptop equivalence** ("single-operator trust makes them
  the same authority", `mesh-provider-and-principal.md` § Identity) is the
  sentence a scope tier supersedes.
- **Reserved seams inventory** (everything that keeps the retrofit cheap):
  `Principal` "future variants additive"; `tokens.kind` virtual column +
  index already in DDL; JSON documents + tolerated unknown fields; `v`
  skip-if-newer (fail-closed skip); rollout-ordering law; additive-DDL-only
  (new generated columns/indexes allowed); Keeper-owned firewall as an
  existing enforcement substrate; the api fold as the single HTTP choke
  point; reseed as the compaction/major-change event.
- **Spec inconsistency:** VISION/backbone both assert "rows carry writer
  identity and timestamp", but neither `corrosion-schema-v1.sql` nor the
  row-model document conventions define any such field for
  operator-authority rows — the claim is currently true only structurally
  (keying) for machine testimony. The signing retrofit's "kept open" claim
  leans on a field that does not yet exist in the drafts.
- Research corpus: only
  `docs/research/openship-multi-server-networking-paths-2026-07-31.md` §
  Constraints and decision points touches this — "Ployz v2 deliberately
  equates membership with config write authority … Hostile-edge and
  multi-tenant membership require a later operator-signing tier … OpenShip
  must not present one Ployz cluster as a hostile multi-tenant boundary."
  Nothing in `.scratch/corrosion-spike/` addresses gossip auth; the spike
  ran stock Corrosion inside WG with no auth configured.
