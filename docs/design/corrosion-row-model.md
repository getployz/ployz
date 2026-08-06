# The Corrosion Row Model

First-draft spec from the wayfinder ticket [Decide: the row model — tables,
ownership map, and the row-ownership law](https://github.com/getployz/ployz/issues/785).
The companion DDL draft is [corrosion-schema-v1.sql](corrosion-schema-v1.sql).
This governs the coreless v2 design; it replaces the intent-file / evidence-log
storage rules of the retired v1 design.

Schema v1 is the first writable contract. No product path writes these v2
documents yet, so required provenance, typed identifiers, transport separation,
and canonical timestamps are part of v1 directly. There is no legacy v2 row
shape to migrate or accept.

## The admission lens

A table earns a Corrosion row only if someone must watch it live: the
gateway, DNS, Cloud, or another machine. Everything else stays local.
Deliberately outside Corrosion:

- Per-machine desired container specs (deploy inputs) — machine-local
  storage, as in Uncloud's `machine.db`. Nobody watches them.
- Operation detail — driver-local JSONL, streamed over SSE on demand.
- WG private keys, TLS keys, secret env values, deploy-time secret
  payloads — never rows in any form.

## The row-ownership law

> Every row has exactly one **authority**: either the operator's command
> stream, or exactly one machine. Machine-authority rows are keyed so that no
> two machines can ever address the same row. LWW therefore only ever
> adjudicates the operator racing themselves — never machine-vs-machine,
> never machine-vs-operator.

Operator authority means a machine writes only while executing the
operator's explicit command; Keeper and background loops never touch
operator-authority rows. Operator self-races are priced in (the accepted
LWW un-happening position): Cloud serializes its own writes through its job
runner, the CLI is in one operator's hand.

## Ownership map

| Table | Authority | Writer in practice | PK | Swept by |
|---|---|---|---|---|
| `cluster` | operator | `ployz init`, settings commands | `ClusterId` | never (identity) |
| `machines` | operator | join/admission, machine rm | `MachineRowId` | `machine rm` |
| `peers` | operator | join/admission, peer rm | `PeerId` | `peer rm` |
| `tokens` | operator/Cloud | token create/revoke | `TokenId` | `token revoke` / refound |
| `namespaces` | operator | namespace create/rm | `NamespaceRowId` | namespace rm |
| `services` | operator | deploy promotion | `ServiceRowId` | superseding deploy |
| `route_bindings` | operator | route attach/detach | `RouteBindingRowId` | route rm |
| `containers` | machine | the machine running the container | Docker container id | owning machine; `machine rm` |
| `machine_status` | machine | the machine itself | `MachineRowId` | `machine rm` |
| `operations` | machine | the executing machine | `OperationRowId` | never in v1 (refound compacts) |
| `cert_holdings` | machine | the holding gateway | `<MachineRowId>:<hostname>` | owning gateway's tick; `machine rm` |
| `acme_http01` | machine | the issuing gateway | ACME challenge token | issuer on order settle; `machine rm` |

## Primary-key discipline

- Every operator-minted row identity is a domain-typed canonical ULID: exactly
  26 uppercase Crockford Base32 characters. Every cross-row reference uses the
  corresponding Corrosion identity type. Human names remain handles used for
  lookup and lowest-ULID claim resolution; they are never row identities or
  references. The v1 types are `ClusterId`, `MachineRowId`, `PeerId`,
  `TokenId`, `NamespaceRowId`, `ServiceRowId`, `OperationRowId`, and
  `RouteBindingRowId`.
- These Corrosion identity types are the v2 row contract. The incumbent
  subject-token identifiers remain frozen and are not accepted in Corrosion
  keys or references.
- Natural writer-scoped keys remain for testimony where the external fact is
  the identity (Docker container id and ACME challenge token). Composite
  testimony keys embed typed canonical machine identity and are never reused by
  construction.
- Never-reused PKs everywhere keeps every table reaper-eligible later
  (a reaped-then-reused PK corrupts the cluster).
- Identity fields are write-once: a row's ULID, a machine's WG public key
  at admission, a token's hash. Mutation of identity = delete + new row
  with a new ULID. Route Binding Identity holds: detach + recreate is a
  new identity even for the same hostname.
- An operation's terminal write is final (at most three summary-state writes
  per op: created, optional running, terminal). Heartbeat refreshes are
  excluded from that count: the executing machine — the row's one writer —
  may rewrite the document's top-level `heartbeat_at` between summary-state
  writes so readers can judge driver liveness. The heartbeat mutator refuses
  terminal documents and can change no other field, so terminal stays final.
  `operations.heartbeat_at` and `containers.deploy` ride as generated columns
  in the DDL.

## Uniqueness without a coordinator: optimistic claims

This is the product-wide coordination primitive (fixed by the unified-cert
ticket, #792, amending #785). There is no lock and no true mutual
exclusion under multi-writer LWW — only a courtesy race-narrowing step
with a deterministic backstop. No quorum locks exist anywhere in the
product: a quorum lock stalls exactly when a majority is unreachable,
the repair-before-command failure class the converged-over-coordinated
thesis rejects.

1. Read: name/hostname free? If taken, refuse normally.
2. Insert the row (the row itself is the claim — no claims table).
3. Courtesy re-read after a fixed short beat (1–2s). This narrows the
   everyday race window; it carries no correctness weight. (An earlier
   draft derived the wait from Corrosion `/v1/health` p99 lag; deleted —
   unproven, and quiet-cluster lag semantics are unknown.)
4. Re-read all rows for the claimed name: lowest ULID wins. Mine → claimed.
   Another → I lost; delete my row, report who won.

Reader law (where safety actually lives): every reader of a named table —
gateway, DNS, Cloud, CLI — resolves duplicates by lowest ULID and
surfaces losers as conflicts (`doctor` names the shadowed row and the
command that removes it). A partition longer than the beat yields a
visible repair, never silent merged truth. A future additive layer
(version-vector ack round for rare cluster-singleton mutations) is noted
but not built.

Machine and peer claims are eligible for that lowest-ULID fold only after a
roster-specific reader has checked their transport against the accepted
`ClusterDocument.provider`. A provider-mismatched row is skipped and surfaced;
it cannot win a name or subnet claim and shadow a valid row.

## Sweep and retention

- **Author-sweeps-own.** The only writer of a row is its only deleter.
  No background deleter exists; every delete happens inside a command or
  the machine's own testimony maintenance.
- **Removal transfers sweep duty.** The command that deletes a machine's
  roster row also sweeps every testimony row that machine authored
  (`machine_status`, `containers`). Deleting the author is the one act
  that lets operator authority touch machine-authority rows.
- **Never swept in v1:** `operations` rows (evidence; compact at refound).
  Expired tokens are invalid at point of use, deleted only by `token revoke`
  (verification is an O(1) lookup by the embedded ULID, so deleting the row is
  itself revocation — one act, no separate `token rm`; see the token/status/
  doctor UX spec, #793) or refound.
- No Corrosion reaper in v1. Tombstones ride to refound — teardown +
  fresh install + re-declared intent (#798) is the escape hatch for
  corrosion upgrades and destructive schema, and thereby also the
  compaction event. There is no reseed and no dump format.

## Cross-cutting conventions

- **`cluster_id` in every document.** A ULID minted by `ployz init`,
  inert in v1; the cells seed and the stray-node data fence.
  Readers drop foreign-`cluster_id` rows and surface them in `doctor`.
- **Provenance in every operator-authority document.** `cluster`, `machines`,
  `peers`, `tokens`, `namespaces`, `services`, and `route_bindings` serialize
  the shared `OperatorWriteProvenance` fields at the document top level:
  `written_by` is the nested `OperationInitiator` shape for the authenticated
  `Principal` (`machine`, `peer`, or `api_token`, with its typed id), for example
  `{"kind":"peer","peer_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV"}`.
  `written_at` is a `CorrosionTimestamp`. The initiator is the authenticated
  principal for the explicit command that wrote the row. Missing or malformed
  provenance makes the document unparseable, so readers skip and surface it
  rather than invent attribution.
- **One timestamp type everywhere.** Every Corrosion document time, including
  operation state times, testimony observations, token lifetime, deploy time,
  certificate issue and expiry, and operator `written_at`, is a
  `CorrosionTimestamp`. It accepts a valid RFC 3339 instant with an explicit
  offset and serializes it as UTC with exactly nine fractional digits:
  `2026-08-04T10:30:00.000000000Z`. Comparisons use the parsed instant, never
  raw JSON or SQL text. A malformed time makes the containing row unparseable;
  an accepted offset form is normalized on its next serialization.
- **`v` integer in every document, skip-if-newer.** Evolution within a
  version is additive-only; readers parse with unknown fields tolerated
  (no `deny_unknown_fields` on row documents). `v` bumps only when an
  old reader would misinterpret, not merely miss a field. A reader
  seeing a newer `v` skips the row and reports it, never guesses.
- **Rollout ordering law.** Binaries roll to every machine before any
  writer emits a new field, new `v`, or new table. DDL is near-frozen:
  new tables and generated index columns only, riding the same rule.
- **Reader guards.** Skip rows whose document is empty or unparseable
  (partial replication mid-sync), whose typed ids or references are
  noncanonical, or whose roster transport disagrees with the cluster provider
  — not-yet-arrived or invalid, never truth. Surface every skipped row. Never
  fold from an empty roster (the WG-lockout guard).

## No secret values

No secret value ever enters a Corrosion row. A Corrosion dump must be
shareable with support without a single credential in it.

- Env: `services` documents carry `env_fingerprints` —
  `{ KEY: sha256 hex of the JSON-encoded value }`, matching Cloud's
  existing fingerprint algorithm, so desired-vs-actual env diffs by
  string equality without values replicating. (Unsalted sha256 of a
  low-entropy value is dictionary-guessable by mesh members — priced in;
  membership is the trust ceiling.)
- Tokens: issued string `pz_<token-ulid>.<32-byte-random-base64>`, shown
  once. The row keeps `sha256(secret part)`; verification is O(1) lookup
  by embedded ULID + constant-time compare. Plain sha256 is deliberate:
  256-bit random secrets, not passwords.
- The rule has **zero exceptions**. The unified-certificate ticket
  (#792) closed the once-pending sealed-cert-key exception as
  unnecessary: TLS keys are machine-local on holders and never rows in
  any form.

## Certificates

Fixed by the unified-cert ticket (#792). One hostname, one certificate,
unified across gateways (per-gateway *independent* issuance stays
vetoed); coordination is the optimistic-claim primitive above, never a
lock.

- **Key material is machine-local on gateways only.** Never rows, never
  in the join payload, never through Cloud. Cert material is
  reproducible — one ACME round-trip recreates it — so it needs no
  durable cluster storage and no seal key exists.
- **Possession is testimony:** `cert_holdings`, one row per
  (gateway, hostname), written only by that gateway when it issues or
  fetches. Readers derive everything: current cert = fingerprint from the row
  with the chronologically greatest typed expiry; fetch sources = the machines
  whose rows carry it. A holders array on a shared row is forbidden —
  membership lists decompose into per-member rows or LWW eats concurrent
  additions.
- **Distribution is holder-to-holder mesh fetch** driven by the named
  set in `cert_holdings` (the known-set gather law), never broadcast
  discovery. A gateway that sees a fresh row but can reach no holder
  keeps fetching and **never issues** — degraded is not entitled to a
  duplicate (protects any CA's duplicate limits).
- **Renewal is lock-free:** a daily tick with a stable per-machine
  jitter offset derived from the machine ULID. Renew when a direct-mode
  hostname is missing a cert or under 1/3 lifetime remains
  (CA-agnostic; no fixed 30-day constant). Re-read `cert_holdings`
  first — fetch if a fresher cert exists, else issue. Duplicate
  issuance under a race is tolerated as harmless; chronologically greatest
  typed-expiry adjudication converges readers. The same tick deletes the
  machine's own holding row and local key material when the hostname no longer
  has a direct-mode binding.
- **HTTP-01 rides public rows:** `acme_http01`, key authorizations are
  public by design. The issuing gateway writes the token row, waits for
  local visibility plus a courtesy beat before triggering validation
  (gossip must outrun the CA's fetch), and sweeps it when the order
  settles.
- **ACME accounts are per-issuing-gateway** — free, reproducible, key
  on own disk, no shared secret, orphans on `machine rm` are harmless.
  CA directory URL and optional contact are public fields on the
  `cluster` document. The named upgrade path to a shared account (for
  EAB CAs, DNS-PERSIST-01 `accounturi` pinning, CAA account pinning):
  a public `acme_account` holdings row plus the same holder-to-holder
  key fetch — additive, not built.
- **Cert scope derives from ingress mode on the route binding.** Direct
  hostnames get the full machinery; Cloudflare Tunnel / Tailscale
  Funnel hostnames get none (the provider terminates TLS). A mode flip
  is repaired by the ordinary tick noticing the gap.
