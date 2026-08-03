# Scoped authority: capability and token systems

Snapshot: **2026-08-01**. Question: which offline-verifiable
scoped-credential systems fit a coreless cluster — verification local to
every machine (no policy server, no CA round-trip, no quorum), revocation
that may converge late under partition, no secret values in replicated rows,
a single operator minting from a CLI, and the smallest mechanism that works?
Systems: Biscuit, macaroons, UCAN, NATS decentralized auth, and briefly
SPIFFE/OPA/Zanzibar. Method: primary sources fetched directly (docs.rs,
GitHub spec repos, NATS docs, Fly.io engineering posts, spiffe.io);
biscuitsec.org blocks automated fetch, so its revocation guide was pulled
via curl. Facts not confirmed from a fetched page are flagged
`[unverified]`. Synthesis:
[scoped authority and the signing tier](../design/scoped-authority-and-the-signing-tier.md).

---

## 1. Biscuit (Eclipse Biscuit, ex-CleverCloud)

**(i) Trust root and key hierarchy.** Single root keypair — Ed25519 or ECDSA
P-256 (both supported in biscuit-auth 6.0.0). A token is a chain of blocks:
an authority block plus attenuation blocks. Each block's signature covers
its content **and the next block's ephemeral public key**, so blocks can be
appended but never removed or altered. No hierarchy of intermediate keys is
required; the verifier needs only the root *public* key. Tokens can carry a
`root_key_id` hint so a verifier holding several accepted root keys picks
the right one (this is the rotation hook).

**(ii) Minting and offline verification.** Mint: sign an authority block
(Datalog facts like `right("namespace-x", "services", "write")`, checks like
`check if time($t), $t < 2026-09-01T00:00:00Z`) with the root private key.
Verify: any machine holding the root public key parses the block chain,
verifies each signature, then runs a local Datalog `Authorizer` that
combines token facts/checks with verifier-side facts (resource, operation,
time, caller) and allow/deny policies. Fully local — "decentralized
validation: any node can verify tokens using only public information"
(docs.rs). No round-trip of any kind.

**(iii) Delegation/attenuation.** Offline attenuation by any holder: append
a block containing additional *checks* (never new rights — appended blocks
can only restrict; the Datalog semantics enforce that non-authority blocks'
facts aren't trusted for rights). E.g. take a full-access token and derive
one limited to one resource, one operation, short expiry — no issuer
contact. **Sealed tokens**: a final seal signature freezes the token so no
further blocks can be appended. **Third-party blocks**: a block signed by an
*external* keypair (not derived from the chain); verifier policies can say
`check if fact(...) trusting {external_pubkey}` — a decentralized analogue
of macaroon discharge without the online dance at verification time.

**(iv) Revocation under partition.** Every block has a unique **revocation
identifier** derived from its signature — unique even for identical content
re-minted. A token exposes the revocation ids of *all* its blocks
(`Biscuit::revocation_identifiers()`), so an attenuated child token always
contains its parent's revocation ids: revoking a parent id revokes every
derived token — the biscuitsec revocation guide states this is deliberate,
"revoking a token should also revoke all derived tokens (else it would be
trivial to circumvent revocation)." Mechanics: the authorizer is given a
local revocation list and refuses matching tokens. **Distribution of that
list is explicitly out of scope / external state** ("revocation requires
external state management" — biscuit-rust README). The guide's suggested
patterns: read list at startup, poll it, download diffs, or queue-based
push. Failure mode under partition: a machine with a stale list accepts a
revoked token until the list converges — exactly a late-converging
revocation. Expiry checks (`time($t)` checks) are the bounded backstop.

**(v) Root rotation.** Multiple accepted root keys via `root_key_id` + a
`KeyProvider`-style lookup on the verifier: add new root, mint new tokens
under it, keep verifying old ones until expiry, drop old root. No
cross-signing machinery; the accepted-roots set is verifier-local config (in
Ployz terms: a non-secret public-key row).

**(vi) Rust maturity and cost.** `biscuit-auth` 6.0.0 (Apache-2.0), the
**reference implementation**; project moved to the Eclipse Foundation
(eclipse-biscuit org), maintainers Geal (Geoffroy Couprie) and divarvel; 243
stars, 788 commits; also C bindings (biscuit-capi), plus
Haskell/Go/Java/JS/Python/C#/Wasm implementations. No formal cryptographic
audit yet ("cryptographic audits remain sought"). Cost per the project's own
authorization-performance recipe: whole pipeline (parse + signature
verification + Datalog build/eval) "usually clocks in at around one
millisecond"; signature verification dominates and scales with block count
(Ed25519 verify is <100µs per block); `Authorizer::execution_time()` exposes
the Datalog share. Production adopters: **Clever Cloud** (Biscuit auth/authz
plugin for Apache Pulsar — per-customer namespace tokens attenuated down to
per-topic produce/subscribe tokens, i.e. exactly the "may write only rows in
namespace X" shape), **3DS Outscale** (IAM), **Space and Time**,
**nixbuild.net**.

**(vii) Fit verdict.** Biscuit satisfies the constraints almost
point-for-point: local public-key verification, no secrets in any replicated
row (only the root public key and a revocation-id list, both non-secret),
holder-side attenuation gives the operator's CLI free least-privilege
minting, and revocation-list rows converging late is precisely the
distribution model the Biscuit docs assume. Risks are the Datalog engine
being a bigger mechanism than a fixed claim struct (second-system smell if
you only need three scopes) and the absent formal audit.

Sources: [biscuit-auth docs.rs](https://docs.rs/biscuit-auth/latest/biscuit_auth/),
[eclipse-biscuit/biscuit-rust](https://github.com/eclipse-biscuit/biscuit-rust),
[eclipse-biscuit/biscuit spec](https://github.com/eclipse-biscuit/biscuit),
[Revocation guide](https://www.biscuitsec.org/docs/guides/revocation/),
[Authorization performance](https://doc.biscuitsec.org/recipes/authorization-performance),
[Eclipse Biscuit proposal](https://projects.eclipse.org/proposals/eclipse-biscuit),
[CleverCloud/biscuit-pulsar](https://github.com/CleverCloud/biscuit-pulsar),
[Clever Cloud intro](https://www.clever.cloud/blog/engineering/2021/04/12/introduction-to-biscuit/)

---

## 2. Macaroons

**(i) Trust root and key hierarchy.** A **symmetric** root key per issuing
service (Fly.io: per-organization HMAC keys). Macaroon = identifier/nonce +
ordered caveat list + tag, where `tag₀ = HMAC(root_key, id)` and
`tagₙ = HMAC(tagₙ₋₁, caveatₙ)` — each caveat's HMAC key is the previous
HMAC's output ("the key for that HMAC is the output of the last HMAC" —
Fly). No key hierarchy; just the root secret.

**(ii) Minting and verification.** Mint requires the root key. Verify
requires **recomputing the whole HMAC chain — which requires the root key**.
This is the structural flaw for a distributed verifier set: *"Macaroons rely
entirely on symmetric cryptography, so anything that can directly verify a
Macaroon can also mint new ones"* (Fly's macaroon-thought.md), and *"If you
can verify a Macaroon, you can generate one. We have thousands of servers.
They can't all be allowed to generate tokens."* Fly's fix was to
**centralize verification** in a physically isolated token-verification
service (tkdb, secrets reachable only over Noise-protocol connections,
LiteFS-replicated SQLite for global reads, client-side result caching) —
i.e., macaroons at scale reintroduced a verification service.

**(iii) Delegation/attenuation.** Best-in-class and trivially offline: any
holder appends a caveat and computes the new tag from the old tail; caveats
can only be added, all must independently hold, so attenuation only ever
weakens. Third-party caveats: the minter embeds a fresh caveat root key
encrypted for a third party; the holder must fetch a **discharge macaroon**
from that third party (which may add its own caveats) and bind it to the
root macaroon before presenting both. Fly implemented it (tickets,
challenges, ephemeral keys, VIDs/CIDs) and concedes *"This all sounds
convoluted."*

**(iv) Revocation under partition.** Nothing built in. Practices:
short-expiry caveats appended at use time, plus issuer-side revocation lists
— Fly identifies every macaroon by unique nonce and revokes by nonce,
invalidating fleet-wide verification caches. Since verification is already
centralized in their design, revocation is immediate there; in a
*distributed*-verifier macaroon design, revocation lists have the same
late-convergence property as any list, but you also still have the
root-secret-everywhere problem.

**(v) Root rotation.** Rotate the per-service/per-org HMAC key and re-mint;
old tokens die with the old key. Key lookup is by an id carried in the
macaroon nonce (Fly: user/org ID → key row). Simple, but every rotation is a
mass invalidation unless you keep both keys accepted.

**(vi) Implementation maturity and cost.** Reference `rescrv/libmacaroons`
is C (513 stars, ~90 commits, essentially dormant), Python bindings. Fly
built their own from scratch in Go + Elixir ("didn't build on any existing
Macaroon code") with structured MsgPack caveats because the open-source
ecosystem's caveats are untyped opaque blobs with no shared language. Rust:
only thin community crates of low maintenance `[unverified — no active,
widely-used Rust macaroon crate surfaced]`. Verification cost is the
cheapest of all options — a handful of HMAC-SHA256s, sub-microsecond — but
only where the secret lives. Adoption verdict from Fly themselves: *"more
talked about than implemented, which is a nice way to say that practically
nobody uses them."*

**(vii) Fit verdict.** Macaroons directly violate the
no-secrets-on-verifiers constraint: local verification on every machine
means the root HMAC secret on every machine, making every peer a minting
oracle — the exact property that forced Fly to build a central verification
service, which a coreless design forbids. The attenuation ergonomics are the
part worth stealing, and Biscuit is explicitly "macaroons with public keys"
— the same offline attenuation without the symmetric trap.

Sources: [Macaroons Escalated Quickly](https://fly.io/blog/macaroons-escalated-quickly/),
[Operationalizing Macaroons](https://fly.io/blog/operationalizing-macaroons/),
[superfly/macaroon macaroon-thought.md](https://github.com/superfly/macaroon/blob/main/macaroon-thought.md),
[rescrv/libmacaroons](https://github.com/rescrv/libmacaroons),
[Fly access tokens docs](https://fly.io/docs/security/tokens/)

---

## 3. UCAN (ucan-wg)

**(i) Trust root and key hierarchy.** No CA: every principal is a **DID**
(mandated `did:key`; Ed25519/P-256/secp256k1 required algorithms). The trust
root for a resource is the resource *subject's own DID* — authority
originates from the entity that owns the thing. Delegations are signed
tokens `iss → aud` forming a proof chain back to the subject.

**(ii) Minting and offline verification.** The subject (or any delegate)
signs a Delegation token (DAG-CBOR encoded, addressed by CIDv1). To act, a
peer presents an **Invocation** (distinct token type, unique CID/nonce to
prevent replay) plus the proof chain. Verification is local: check each
signature link, check `iss`/`aud` continuity, check time bounds (`exp`
recommended, ±60s clock-drift buffer suggested), check attenuation validity.
No server anywhere.

**(iii) Delegation/attenuation.** Each link "MUST directly restate or
attenuate (diminish) its capabilities." Capabilities are
`subject + command + policy`; commands are path-shaped (`/crud/read`),
shorter paths subsume longer ones, `/` is the top. Delegation is explicitly
"idempotent and partition-tolerant"; invocation is the effectful,
replay-protected act. This delegation/invocation split is UCAN 1.0's main
structural difference from Biscuit/macaroons.

**(iv) Revocation under partition.** The most partition-honest spec of the
group: any issuer *appearing anywhere in a proof chain* may revoke a
downstream delegation by CID, via a `ucan/revoke` invocation (with optional
chain-witness `path` to prevent DoS). Revocation records are **append-only,
idempotent, "highly amenable to caching and gossip"**; resource controllers
MUST keep a revocation cache; stores may be centralized or "embedded
directly (e.g., CRDT-based file systems)"; out-of-order delivery is fine,
revocations arriving before their target delegation are valid, and **no
delivery-time bound is required** — i.e., stale-truth-repaired-later is the
designed model. But the revocation subspec is only **v1.0.0-rc.1**.

**(v) Root rotation.** Weakest point: a `did:key` *is* the key — rotating
the subject's root key means a new DID and re-issuing every delegation chain
from scratch (mitigations like `did:plc`/`did:web` indirection add an
external resolution dependency, which breaks pure-local verification).

**(vi) Rust maturity and cost.** `rs-ucan` (ucan-wg org): v0.8 on crates.io,
targets **v1.0.0-rc1**, README banner "⚠️ Work in progress ⚠️", "not been
formally audited. Use at your own risk!" — real but pre-production. The
battle-tested implementation is TypeScript (`ucanto`, run in production by
web3.storage/Storacha as the authorization layer for their decentralized
storage network; its ideas fed UCAN 1.0). Fission, the spec's originating
company, wound down `[unverified — searches did not confirm the shutdown
date]`; the spec now lives under the ucan-wg community org and core spec is
tagged 1.0.0. Verification cost: one signature verify **per link in the
proof chain** plus CBOR/CID hashing and policy checks — a 3-link chain ≈ 3
Ed25519 verifies, comparable to Biscuit, but chains and CID plumbing make
the implementation surface larger.

**(vii) Fit verdict.** Semantically the closest match to "capability rows
gossiped in a CRDT store" — its revocation model is literally designed for
eventually-consistent gossip — but the Rust implementation is a WIP release
candidate and the DID/CID/DAG-CBOR stack drags in an IPFS-flavored second
system for what a single signed struct could express. Right ideas,
wrong-weight machinery for a single-operator small cluster today.

Sources: [ucan-wg/spec](https://github.com/ucan-wg/spec),
[ucan-wg/revocation](https://github.com/ucan-wg/revocation),
[ucan-wg/rs-ucan](https://github.com/ucan-wg/rs-ucan),
[storacha/ucanto](https://github.com/storacha/ucanto),
[Storacha UCAN docs](https://docs.storacha.network/concepts/ucan/)

---

## 4. NATS decentralized auth (the production "coreless auth" analogue)

**(i) Trust root and key hierarchy.** Three-tier chain of **NKeys**
(Ed25519, prefix-encoded: `O` operator / `A` account / `U` user public keys,
`S...` seeds): the **operator** JWT is embedded in every server's config
(the pinned trust root), operator signs **account** JWTs, accounts sign
**user** JWTs. "In the hierarchy, signing keys can only be used to sign JWT
for the role right below them." Each entity has an immutable **identity
key** plus optional mutable **signing keys** listed inside its JWT ("Signing
NKEYs... unlike identity NKEY may change over time").

**(ii) Minting and offline verification.** Minting is a pure CLI act (`nsc`
/ `nats auth`) on the operator's laptop — no service involved. At client
connect, a server performs three checks **with zero external auth calls**:
(1) client signs a fresh server nonce with its user seed —
proof-of-possession, so user JWTs are not bearer tokens (unless the explicit
`--bearer` mode for browser clients, which drops the nonce check); (2) user
JWT signature chains to the account's identity or signing key
(`issuer_account` names the account when a signing key issued it); (3)
account JWT (obtained from the local resolver) chains to the operator key in
server config. "All verification is deterministic and offline — no external
auth service is required... a signature check works for a user the server
has never seen."

**(iii) Delegation/attenuation — scoped signing keys.** The
scoped-credential mechanism: an account **scoped signing key** carries a
role name and a pinned permission template (publish/subscribe allow/deny
lists, limits). Every user JWT issued by that key gets *exactly* that scope,
applied server-side at connect time (the user JWT itself stays empty of
permissions). Consequences documented by NATS: "a leaked signing key can
only issue users with the scope you already chose" (bounded blast radius),
and editing the scope re-scopes **all** users issued by that key on the next
account push — role-wide policy change without reissuing user credentials.
This is delegation-by-issuer, not holder attenuation: a *user* cannot
further attenuate their own JWT offline.

**(iv) Revocation under partition.** Revocations live **inside the account
JWT** as a `revocations` map of user public key → Unix timestamp; a server
rejects any user JWT by that key with `iat` ≤ the timestamp (a later
`issuedAt` is accepted — enabling clean re-issue under the same identity...
in practice `nats auth` re-issues under a new key). A wildcard entry revokes
all users issued at/before a time. The same mechanism exists for export
activation revocations. Flow: edit account JWT locally → `push` to the
resolver → servers holding the update **immediately disconnect** matching
clients. Distribution is the **account resolver**: *memory* (JWTs preloaded
in server config, reload without restart — fine for small static account
sets), *full/NATS-based* (recommended for production: every server persists
JWTs on disk and "will gossip missing JWTs in an eventually consistent way;
servers without a copy will perform a lookup from servers that do"), *cache*
(LRU subset, needs a full resolver behind it), *URL* (external
nats-account-server, legacy). **Partition failure mode: a partitioned server
keeps enforcing its last-seen account JWT — revocation converges only when
gossip does.** NATS ships this as acceptable; expiry is the backstop, with
the guide's rule: "For sign up service issued JWTs, ALWAYS set the SHORTEST
POSSIBLE EXPIRATION."

**(v) Root rotation.** The known sore spot: the operator **identity** key is
unrotatable in practice — "Operator seed loss: no recovery path — accounts
cannot be re-signed without it"; compromise recovery means migrating every
account to a new operator with little tooling help. Documented mitigation:
**use operator signing keys from day one, keep at least one offline**, so
rotation happens among signing keys while the identity key stays cold.
Account signing keys rotate freely (add/remove keys in the account JWT,
push); user JWTs stay valid because validation accepts any current account
signing key. Removing a signing key is mass revocation of everything it
issued.

**(vi) Implementation and cost.** Server is Go, but the *pattern* is fully
portable: Rust has first-class `nkeys` and JWT handling in the async-nats
ecosystem. Per-connection verification cost = 2–3 Ed25519 signature verifies
plus JSON claim checks (~100–300µs), amortized over the connection lifetime.
Years of production use across NGS/Synadia and self-hosted operator-mode
deployments — this is the most battle-tested "no central verifier" design
in the list.

**(vii) Fit verdict.** NATS proves the exact coreless shape works in
production: a CLI-held root, an offline signature chain checked by every
node, scoped signing keys as roles, and revocation as data (a map inside a
signed doc) gossiped eventually-consistently — swap "account JWT in the
resolver" for "signed roster/authz row in Corrosion" and it's the same
machine. Its lessons transfer directly: proof-of-possession over bearer,
scoped keys to cap blast radius, short expiry on machine-minted creds, and
an offline root signing key from day one because identity-key rotation is
the one thing it never solved.

Sources: [NATS decentralized auth](https://docs.nats.io/learn/security/decentralized-auth),
[In-depth JWT guide](https://github.com/nats-io/nats.docs/blob/master/running-a-nats-service/nats_admin/jwt.md),
[Account resolver docs](https://docs.nats.io/running-a-nats-service/configuration/securing_nats/auth_intro/jwt/resolver),
[resolver.md source](https://github.com/nats-io/nats.docs/blob/master/running-a-nats-service/configuration/securing_nats/jwt/resolver.md),
[Memory resolver tutorial](https://docs.nats.io/running-a-nats-service/configuration/securing_nats/auth_intro/jwt/mem_resolver)

---

## 5. Briefly: SPIFFE/SVID, OPA, Zanzibar

**SPIFFE/SVID.** Workload identity: `spiffe://trust-domain/path` IDs carried
in **SVIDs** (X.509 certs preferred, JWT-SVIDs for L7-proxy paths);
verification is local against a distributed **trust bundle** of CA roots;
revocation is handled by making SVIDs short-lived with automatic rotation
via the Workload API. The catch: SPIFFE's practical implementation is
**SPIRE, a server/agent architecture — the SPIRE server is a running CA that
nodes attest to for every issuance/rotation cycle**. Verification is offline
but the *identity supply chain* is a central service with a heartbeat; and
SPIFFE is identity-only — "may write services rows in namespace X" still
needs an authorization layer on top. Two systems where the constraint allows
at most one.
([SPIFFE concepts](https://spiffe.io/docs/latest/spiffe-about/spiffe-concepts/))

**OPA-as-sidecar.** The PDP itself is local (sidecar evaluates Rego
in-process, no per-request round-trip), so it technically passes local
verification — but OPA is a policy *evaluator*, not a credential system: it
answers "is this request allowed" given data it must be fed, so an
offline-verifiable way to establish *who is asking with what grant* is still
needed, plus a bundle-distribution pipeline for policy. A second system
(Rego language, bundle plumbing) delivering less than one signed capability
struct checked in Rust.

**Zanzibar.** Google's relationship-tuple authorization DB: correct answers
depend on a **globally consistent, replicated, quorum-backed store with
snapshot tokens (zookies)** to defend against the "new enemy" problem. It is
definitionally a central (if replicated) policy service with consistency
requirements — a direct violation of "no policy server, no quorum," and
pointed at the multi-tenant many-principal problem the single-operator
ceiling doesn't have.

---

## Comparison table

| | Biscuit | Macaroons | UCAN 1.0 | NATS operator/account/user | SPIFFE/SPIRE |
|---|---|---|---|---|---|
| Trust root | Root keypair(s), Ed25519/P-256; `root_key_id` selects | Symmetric HMAC root key per issuer | Resource subject's DID (did:key) | Operator NKey pinned in server config | Trust-domain CA bundle (SPIRE server = CA) |
| Offline verify on every machine | Yes — root *public* key only | **No** — verifier holds root secret ⇒ can mint (Fly centralized verification because of this) | Yes — chain of pubkey sig verifies | Yes — 2–3 Ed25519 verifies vs pinned operator key + resolver-cached account JWT | Verify yes (bundle); **issuance/rotation needs live SPIRE server** |
| Secrets required on verifiers | None | Root HMAC key (fatal) | None | None (account JWT is signed, non-secret) | None for verify; agent/server keys for issuance |
| Scoped credential | Datalog checks in authority block | Caveat list | Capability = subject+command path+policy | **Scoped signing key** = role template applied at connect | None — identity only, authz is BYO |
| Holder attenuation offline | Yes (append blocks; sealable; third-party blocks) | Yes (append caveats; third-party via discharge dance) | Yes (re-delegate with diminished caps) | **No** — issuer-side only | No |
| Revocation mechanics | Per-block unique revocation ids; child carries parent's ids; verifier-local list; **distribution external** (fits Corrosion rows) | Nonce-based lists + expiry caveats; no standard | Revoke-by-CID records, append-only, **spec'd for gossip/CRDT stores, out-of-order OK** | `revocations` map *inside* signed account JWT, gossiped by full resolver; disconnect on arrival | Short TTL + rotation only |
| Partition behavior | Stale revocation list ⇒ accepts until list converges; expiry backstop | n/a (central verify) or same-as-list | Explicitly eventual; no delivery bound required | Server enforces last-seen account JWT until gossip converges; short expiry advised | Certs expire; can't renew while partitioned from SPIRE server |
| Root rotation | Multi-root via root_key_id; graceful overlap | Rotate HMAC key, mass re-mint | **Weak: new key = new DID = re-delegate everything** | Identity key unrotatable (use offline signing keys from day one); signing keys rotate freely | CA rotation via bundle updates |
| Rust maturity | **biscuit-auth 6.0.0, reference impl, Eclipse project; no formal audit** | No credible maintained crate; reference is dormant C | rs-ucan 0.8, targets 1.0.0-rc1, "work in progress", unaudited | Pattern portable; Rust nkeys/JWT exist; server itself Go | SPIRE is Go; Rust client libs only |
| Verify cost | ~1ms total (sig verify dominates, per block) | ~µs HMACs (where secret lives) | ~1 sig verify per chain link + CBOR/CID | ~100–300µs per *connection* | TLS handshake path |
| Production proof | Clever Cloud (Pulsar per-topic attenuation), Outscale IAM, Space and Time, nixbuild.net | Fly.io (with a central verification service) | Storacha/web3.storage (via TS `ucanto`) | Synadia NGS + operator-mode fleets, years | Large-scale mesh deployments |
| Constraint verdict | **Best mechanism fit**: local verify, no replicated secrets, CLI minting, revocation-as-rows; watch Datalog weight vs. a fixed claim struct | Disqualified by symmetric verify-⇒-mint on every machine | Right partition semantics, immature Rust + heavy DID/CID stack | **Best architecture precedent**: proves signed-doc + gossip + scoped keys + revocation-as-data works corelessly; not a drop-in library | Central issuing service + identity-only: doesn't fit |

**Cross-cutting takeaway:** two candidates survive the constraints, and
they're complementary rather than competing. NATS is the *architecture*
precedent (a signed configuration document distributed
eventually-consistently, verified against a CLI-held root pinned on every
node, roles as scoped signing keys, revocation as data inside the signed doc
— with short expiry pricing in partition staleness). Biscuit is the
*library* that implements the credential half of that shape in mature Rust
with two properties NATS lacks: holder-side offline attenuation and
derived-token revocation via inherited revocation ids, whose "distribute the
list yourself" gap is exactly a Corrosion row. The minimal composite both
point at: Ed25519 root on the operator's laptop, per-peer scoped credentials
verified locally, revocation ids + root public keys as ordinary non-secret
replicated rows, expiry as the partition backstop, and an offline spare
root/signing key from day one — key-rotation-after-compromise is the one
problem none of these systems solves gracefully after the fact.
