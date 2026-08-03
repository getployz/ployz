# Scoped Authority and the Signing Tier

Research-derived draft, **not a wayfinder decision**. It maps the solution
space for the trust ceiling — membership is write authority, so every
admitted peer or machine holds full command of the cluster — and names the
cheap moves that keep the deferred signing tier honestly open. Companion to
[the mesh provider seam](mesh-provider-and-principal.md) and
[the Corrosion row model](corrosion-row-model.md); the deferred tier's
parent is [#782](https://github.com/getployz/ployz/issues/782). Backbone's
rejection of signing machinery ahead of real demand stands; this document
exists so the eventual decision starts from evidence instead of a blank
page. Raw findings: `docs/research/scoped-authority-*.md`.

## Two problems wearing one name

The trust ceiling blurs two tiers with different attackers and
different-strength fixes:

- **Peers** (operator laptops, Cloud, future plugin controllers) run no
  Corrosion agent. Every peer write funnels through some machine's api
  fold, where Principal resolution already happens — a real, enforceable
  choke point, once its one bypass is closed.
- **Machines** run Corrosion agents. Gossip is multi-writer and
  unstoppable: no process can refuse a hostile member's row before it
  replicates. The only lever is readers refusing to believe — signed rows
  verified at the point of use — and its honest ceiling is that
  impersonation degrades to attributable vandalism, never to prevention.

Third-party integrations — the demand that would trigger this work — are
peers. The machine tier stays deferred; its sketch below records what the
research settled so the eventual ADR is writable on demand.

Nobody in the niche has solved either tier: Fly runs Corrosion on pure
convention (their 2024-09-07 infra-log shows one poisoned row deadlocking
the global proxy fleet in seconds; the fix was rate limits and regional
sharding, not authorization), and Uncloud ships plaintext gossip with no
authority model at all. The production proof that the coreless shape works
is elsewhere: NATS decentralized auth and Tailscale tailnet lock are both
"signed authority statements replicated as ordinary data, verified locally
against an operator-held root, revocation converging late with an expiry
backstop" — a Corrosion row with extra steps, priced exactly like the
stale-truth class the thesis already accepts.

## The gossip-port fence (precondition for everything)

Cryptokey routing admits *all roster peers* — machines ∪ peers — to every
machine's ULA; Corrosion binds the ULA; no spec firewalls its port. Stock
Corrosion's knobs (gossip mTLS, one API bearer token) top out at
membership-grade auth and scope nothing. So today a peer is kept off the
gossip/sync surface only by running trusted software, and any api-fold
authorization is bypassable by dialing Corrosion directly.

The closer: Keeper converges one firewall rule — the Corrosion port accepts
machine ULAs only, never peer ULAs. Roster-driven per-address (machine and
peer /112s share one derived /48 with no structural prefix distinction),
riding Keeper's existing mesh diet and firewall ownership. This belongs in
v1 regardless of any scoping work: it is a hole in the stated fence, not a
feature.

Considered and rejected for the fence: Corrosion gossip mTLS with
machine-only client certs — the same fence plus a CA and cert-distribution
apparatus the product otherwise does not need.

## Scoped peers at the api fold

The peer tier, additively, inside the existing laws:

- **`scope` on the peers document** — a fixed enum
  (`admin | read | writer{namespaces}`), not a policy language — mirrored
  on the tokens document so the join door stamps the scope at admission
  (the door writing an operator-authority row while executing the
  operator's token-backed command is already the sanctioned pattern). The
  DDL already reserves an indexed, unused `tokens.kind` column; peers
  documents tolerate additive fields today. The init path enrolls the
  founding driver's peers row directly and carries the scope the same way.
- **Enforcement at Principal resolution.** The one uniform
  address→identity rule already does the roster lookup; scope arrives with
  it. Handlers that authorize against `Peer` vs `Machine` match one level
  deeper. With the gossip port fenced, *reads* ride the same door — a
  read-only peer is full-read/no-write, and namespace-scoped peers can
  have row-filtered queries and SSE watches, including withholding
  `env_fingerprints` from plugin peers instead of pricing in their
  dictionary exposure.
- **No credential cryptography.** Biscuit/UCAN/JWTs solve "prove who you
  are and what you may do, offline, to a stranger." Ployz peers never meet
  strangers: cryptokey routing already gives unspoofable
  proof-of-possession, and the roster row is the credential. Scope-on-row
  is the NATS scoped-signing-key move restated — blast radius fixed at
  mint time, one edit re-scopes the role. Biscuit (mature Rust, offline
  attenuation, revocation-ids-as-rows) is the shelf pick if a bearer
  credential is ever genuinely needed; macaroons never are (symmetric:
  every verifier is a minting oracle — the property that forced Fly to
  build the central verification service this product refuses).
- **Scope is write-once**, like every identity field: changing a peer's
  scope is `peer rm` + re-admit. This folds scope revocation into the
  existing removal fence instead of inventing a second, weaker one — a
  live-row downgrade would converge with no TTL bound, an unbounded
  variant of the priced token-revocation window.
- **Mixed versions fail closed.** Scoped peers rows carry a bumped `v`:
  old binaries skip-and-report the row, Keeper on a lagging machine never
  converges that WG peer, and the scoped peer is dark to old machines
  rather than silently promoted to full authority. Rollout-ordering law
  first, then mint. Doctor's existing skipped-newer-`v` finding surfaces
  the darkness.
- **Surfacing:** a peer listing surface, scope in `token list`, and a
  doctor finding for a peer some machine has not yet converged — none
  exist yet; doctor's copy-paste-repair pattern is the home.

Supersedes one sentence in the mesh/Principal spec: "Laptop and Cloud
share `Peer` — single-operator trust makes them the same authority."
Admin-scoped laptop and Cloud remain equals; the sentence gains a tier
below them.

Also pinned here: peer joiners must **not** receive the cluster door
private key in the join response (the join spec currently hands it to
"each joiner" without distinguishing machine from peer — a scoped peer
holding it could impersonate the fingerprint-pinned door). Machine joiners
only.

## Keeping the signing retrofit open

VISION and backbone assert "rows carry writer identity and timestamp,"
and the trust ceiling's "retrofit stays open by construction" leans on
that. The drafted schema defines no such field for operator-authority
rows; Corrosion's internal actor_id is spoofable (a claim in the change
message, cryptographically bound to nothing) and no spec claims it. Two
near-free acts make the claim true:

1. **`writer` + `written_at` in operator-authority row documents** — pure
   document addition. Attribution and the promised after-the-fact fold
   surfacing, and the substrate later signatures cover.
2. **Mint the operator root keypair at `ployz init`** — Ed25519, public
   key a field on the cluster document, private key on the operator's
   machine, a spare signing key kept offline. Every surveyed system agrees
   root rotation after the fact is the unsolved problem (NATS's
   unrotatable operator identity; TUF's threshold-signed root chain being
   the only good answer); a root that exists from init, even with zero
   verification built, is what makes the signing tier additive instead of
   a re-keying ceremony across every cluster created in the meantime. Same
   greenfield logic that shipped deny-by-default namespace isolation in
   v1: the wall goes up while nobody lives against it.

## The signing tier, sketched (deferred)

For the eventual ADR, the research settles the shape:

- **What is signed:** `(table, pk, document, writer's own monotonic seq)`.
  The pk inside the signature stops transplanting a signed cell onto
  another row; the sequence plus a per-writer reader-side watermark stops
  replaying the writer's own old row (the TUF/SSB/Hypercore consensus —
  signing the value alone is a known-broken shape).
- **Grants, not ACL rows:** operator-signed scope grants in SPKI style
  ("machine key M may write tables X where the owner key is M") with
  **expiry as the revocation floor** — revocation is ceasing to renew,
  which fits converged-over-coordinated better than revocation lists that
  must outrun a partition. Machines get a dedicated Ed25519 signing key at
  join (WG identity keys are Curve25519; not overloaded). Read-only
  members fall out free: no grant, no belief.
- **The honest ceiling:** cr-sqlite's `col_version` is writer-supplied and
  unbounded, so a hostile gossip member can always make the *stored*
  winner be its row. Signatures make readers refuse to believe it —
  impersonation degrades to vandalism/deletion, attributed and
  doctor-visible, repaired by `machine rm` + refound. The removal fence
  stays the real remedy; signing narrows the window and names the
  culprit. Verified readers keep a shadow of last-verified state so an
  overwrite reads as damage, not truth.
- **Freshness:** per-writer signed heads (a periodically re-signed
  sequence + wall-clock row) let honest readers detect rollback and freeze
  — SUNDR's result that fork-consistency's countermeasure is exactly the
  cross-checking an honest gossip layer already performs.
- **The open question the ticket must decide:** grant issuance under
  token-join, where the operator is not present — a grant template carried
  in the join blob, versus a CSR-shaped "operator countersigns at next CLI
  contact."

## The staleness sentence

Every surviving system names what enforcement does when its authority data
may be stale (Consul `down_policy`, Teleport `strict`/`best_effort`,
Tailscale's last-distributed filter). Ployz's single sentence, stated once
here rather than implied per-feature: **enforcement is always from local
last-converged truth, best-effort; expiry bounds staleness where a TTL
exists; the hard fence is removal.** No strict/fail-closed mode ships — a
machine that must refuse to act because it cannot prove its authority data
is fresh is the repair-before-command failure class the thesis rejects.

## Considered and rejected

- **Macaroons** — symmetric verification puts the minting secret on every
  verifier. Fly's own conclusion, having shipped them, was to centralize
  verification.
- **UCAN** — the right partition-tolerant revocation semantics
  (append-only, gossip-friendly, no delivery bound) wrapped in a
  DID/CID/DAG-CBOR stack with a work-in-progress Rust implementation; an
  IPFS-flavored second system for what one signed struct expresses.
- **Merge-time causal authority** (Matrix state-resolution v2, Keyhive,
  p2panda strong removal) — the strongest semantics (demotions
  retroactively invalidate concurrent writes) but requires a causal DAG
  Corrosion does not have; building a Kleppmann-style hash-DAG in
  application tables is a second database, backdating stays unsolved
  without consensus, and the mature precedent (Matrix) earned its
  subtlety through exploited state-reset bugs.
- **Quorum-cosigned revocation** (tailnet lock's majority-of-signers
  rule) — collides with the no-quorum law, and the single-operator
  ceiling makes it moot: one root, no committee. Multi-signer arrives, if
  ever, with multi-operator demand — not with this tier.
- **OPA / Zanzibar / SPIFFE-SPIRE** — a policy language plus bundle
  pipeline, a quorum-backed relationship store, and a CA-with-heartbeat
  identity supply chain respectively; each a second system for a problem
  three enum variants and one signature check cover.
- **Forking or patching Corrosion** — the sidecar is stock and
  version-pinned; every mechanism above lives in the application layer
  and treats `actor_id`, `col_version`, and row contents as
  attacker-controlled inputs.

## Spec repairs this surfaced

Independent of any decision on scoping itself:

1. The Corrosion port fence (above) — a v1 hole in the stated
   "cannot reach your Corrosion port" reasoning.
2. The writer-identity claim — VISION/backbone assert a field the schema
   drafts do not define.
3. The door private key handed to "each joiner" — restrict to machine
   joiners.
4. `status`/`doctor`/`token list` have no peer-facing surfaces; any
   authority tier needs them, and `peer ls` is unspecced today.
