# Scoped authority: mesh and cluster product survey

Snapshot: **2026-08-01**. Question: how do comparable products scope
authority, and which patterns genuinely survive without a central runtime —
versus secretly depending on one? Products: Tailscale (ACLs and tailnet
lock), Consul, Kubernetes node isolation, Nomad/Serf/memberlist, etcd RBAC,
Teleport, Fly.io's internal Corrosion posture, and Uncloud. Sources: primary
docs and, for Uncloud, source inspection. Synthesis:
[scoped authority and the signing tier](../design/scoped-authority-and-the-signing-tier.md).

## 1. Tailscale

### 1a. ACLs / grants

- **(i) Policy lives:** One huJSON tailnet policy file on Tailscale's
  coordination server (edited via admin console, GitOps, or API). Central
  document, single source.
- **(ii) Enforcement:** Receiver-side, on every node. "A device enforces
  incoming connections based on the access rules distributed to all
  devices... Rule enforcement happens on each device directly, without
  further involvement from Tailscale's coordination server." The control
  plane compiles the policy into per-node views two ways: (a) packet-filter
  rules pushed in the netmap, enforced at decryption time; (b) key
  distribution as capability — each node is given only the public keys of
  peers allowed to reach it, so unauthorized peers can't even complete the
  handshake. Grants add app-layer capabilities: control-plane-evaluated,
  delivered to the *destination* node, whose applications read the caller's
  capabilities (opaque JSON, app-defined semantics) from local state.
- **(iii) Survives control-plane loss:** Data plane is explicitly
  independent ("the control plane... carries virtually no traffic. It just
  exchanges a few tiny encryption keys and sets policies. The data plane is
  a mesh"). Nodes keep enforcing the last-distributed filter; policy
  *changes* and new-node admission stall.
- **(iv) Node self-scoping:** N/A — nodes aren't writers of shared state;
  the only shared truth (the netmap) is authored centrally.
- **(v) Revocation:** Push a new netmap / expire node keys. While control is
  unreachable, a revoked peer keeps its last-granted access —
  late-converging by construction, no explicit staleness policy documented.
- **(vi) Portability verdict:** The *enforcement* pattern (each node
  verifies incoming callers against a locally held, compiled policy) is
  fully portable; the *compiler* is central only as an editor — a coreless
  system can replicate the policy document itself as data and let each
  machine compile its own view.

### 1b. Tailnet lock

- **Model:** Retrofits distrust-of-the-control-plane onto the mesh. Trust
  roots in **Tailnet Lock Keys (TLKs)** held by designated signing nodes
  (max 20), not in Tailscale. Every node carries the **Tailnet Key
  Authority (TKA)**: a replicated, append-only cryptographic chain
  (Authority Update Messages) recording the trusted-key set and its
  changes.
- **What's signed:** A signing node signs each new device's public node key
  with its TLK. The coordination server still *distributes* netmaps, but is
  demoted to a dumb relay: **peer nodes independently verify each peer's
  node-key signature against their local TKA before allowing traffic**.
  "Even if Tailscale were malicious or Tailscale infrastructure hacked,
  attackers can't send or receive traffic in your tailnet." The control
  plane can withhold updates (freeze/DoS) but cannot forge membership.
- **Bootstrap:** TOFU — trust control once at `tailscale lock init`, then
  the trust center moves into the customer's network.
- **Revocation:** `tailscale lock revoke-keys` with **multi-signature
  cosigning** — cosigns must be performed on distinct signing nodes and
  exceed the number of keys being revoked; **fork resolution**: if a
  majority of signing nodes agree a key is revocable, the honest fork wins.
  Removing a signing node re-signs its signees with the remover's key.
  **Disablement secrets** (10 generated at init, any 1 disables the
  feature) prevent the control plane from quietly turning the mechanism
  off; lose them all and the tailnet is unrecoverable.
- **Limitations:** TKA chain grows unbounded (key rotation capped at
  ~1/year to limit it); TLK private keys sit on devices; nodes without
  persisted state must re-fetch TKA at startup (temporary trust window);
  mutually exclusive with device approval; Android can't sign.
- **Partition behavior:** A node that hasn't synced the revocation AUM
  still trusts the old key — late-converging revocation, resolved on chain
  sync; majority-of-signers fork rule bounds the damage.
- **(vi) Portability verdict:** The most portable design in this survey —
  authority is *signed statements replicated as ordinary data and verified
  locally*; a coreless store can carry the same shape (tier-granting keys
  sign membership/authority rows, every machine verifies before honoring a
  caller), and its revocation story (cosigned removal + late convergence +
  majority fork rule) is exactly the tolerance model a coreless design
  needs — minus the quorum, under a single-operator ceiling.

## 2. Consul ACLs

- **(i) Policy lives:** Tokens, policies, roles in the **primary
  datacenter's server Raft store**. Secondaries replicate policies/roles;
  token replication is opt-in (`acl.enable_token_replication`).
- **(ii) Enforcement:** Servers evaluate; **client agents cache resolved
  tokens/policies/roles locally (default TTL 30s each:
  `token_ttl`/`policy_ttl`/`role_ttl`) and enforce from cache** rather than
  round-tripping every request.
- **(iii) Partition behavior — the interesting knob:** `acl.down_policy`
  when servers are unreachable: `allow` (open), `deny` (closed),
  `extend-cache` (default — keep honoring cached grants **ignoring
  expiry**; uncached falls to `default_policy`), `async-cache` (same,
  refresh in background). Enforcement survives on explicitly stale
  authority; the staleness policy is a named operator choice.
- **(iv) Node self-scoping:** Yes — **node identities**: a templated config
  block that auto-generates a policy scoping an agent's token to write only
  its own node data (the agent token). Direct analogue of
  write-own-testimony, but evaluated at the servers.
- **(v) Revocation:** Delete the token at the primary; converges as caches
  expire. Under partition with `extend-cache`, revocation simply does not
  converge until healing — accepted, documented tradeoff.
- **(vi) Portability verdict:** Bearer tokens resolved against an online
  store are not portable (the token store *is* a central plane); what ports
  is the **stale-cache-with-explicit-down-policy** idea — name what a
  machine does when its authority data may be stale.

## 3. Kubernetes Node authorizer + NodeRestriction

- **(i) Policy lives:** In API-server code/config — the Node authorizer is
  a special-purpose graph authorizer; NodeRestriction is an admission
  plugin. Identity lives in the kubelet's **client certificate**: username
  `system:node:<nodeName>`, group `system:nodes` (issued via TLS
  bootstrapping/CSR flow).
- **(ii) Enforcement:** Entirely at the **API server** — authorization
  (Node authorizer: may read own Node, pods bound to it, and
  secrets/configmaps/PVCs *referenced by* those pods; may write own node
  status, its pods' status, events) plus admission (NodeRestriction: object
  being written must belong to the caller's node name; extended in v1.33+
  to restrict which service-account token audiences a kubelet may request).
- **(iii) Survives losing the central plane:** No — and it doesn't need to,
  because the pattern **secretly depends on the choke point**: etcd is
  unreachable except through the API server, so one enforcement door covers
  all writes. That dependency is the non-portable part.
- **(iv) Node self-scoping:** The canonical implementation: **the check is
  a pure function of (caller identity from cert, owner field of the target
  object)** — no per-node policy objects exist at all; the "policy" is one
  rule plus the identity convention.
- **(v) Revocation:** Kubernetes famously has **no certificate revocation**
  (no CRL/OCSP support); mitigations are short-lived certs with rotation,
  and deleting the Node object (which the authorizer graph then fails).
  Partition: the kubelet can't write anything anyway without the API
  server.
- **(vi) Portability verdict:** The *rule* ports perfectly — "identity in
  the credential must equal the owner key of the row" needs no central
  state and can run identically on every machine; the *placement* does not
  port — in a coreless CRDT store the same check must run at **every
  replica's write door and again at gossip-merge time**, or a scoped peer
  just authors rows straight into gossip and syncs around the door.

## 4. Nomad / Serf / memberlist gossip keyring

- **(i) Policy lives:** A shared symmetric keyring file on each member. In
  Nomad, **only servers gossip** — clients never hold the key and talk RPC
  (mTLS + ACLs, tokens in server Raft) instead; in Consul all agents
  gossip.
- **(ii) Enforcement:** At message encrypt/decrypt. Memberlist rule: many
  keys may decrypt, exactly one encrypts. No key → can't join or inject.
  **All key-holders are exactly equal.**
- **(iii) Survives central-plane loss:** Trivially — there is no central
  plane. The purest decentralized mechanism surveyed.
- **(iv) Node self-scoping:** None. Key possession = membership = authority
  to inject any gossip message, spoof any member event. Precisely
  "membership = full write authority" — the model a WireGuard mesh provides
  today.
- **(v) Revocation/rotation:** install → use → remove across the whole
  cluster; the multi-key decrypt window makes rotation non-disruptive.
  **Per-node revocation does not exist** — evicting one member means
  rotating the key on every other member, O(cluster).
- **(vi) Portability verdict:** Proves membership-key auth is a fine
  *outermost gate* and structurally incapable of expressing tiers — don't
  try to bend it into one.

## 5. etcd RBAC (brief)

- Users/roles/permissions stored **in etcd itself**, replicated by Raft —
  policy travels with the data. Every server enforces on v3 gRPC ops.
  Permissions are **key-range intervals `[start, end)`** (prefix =
  interval), read/write/readwrite. Identity: password or **client-cert CN**
  as username. Policy is on every replica, so any surviving server enforces
  — but etcd *is* a quorum store; writes need quorum regardless.
  Self-scoping: give each writer a role whose write range is its own prefix
  — **key-range prefixes are the schema-shaped version of "write only your
  own rows."** Revocation via user/role deletion through Raft; JWT mode
  verifies offline but is then TTL-bound for revocation.
- **Portability verdict:** The quorum store doesn't port, but the **shape**
  does: identity → allowed key-prefix ranges is about the smallest
  expressible write-scoping schema, and "every replica checks before
  applying" is exactly where a coreless store must put the check.

## 6. Teleport (brief)

- **Roles are encoded inside the short-lived certificate** (SSH cert /
  X.509 subject: username + Teleport roles). CA public keys are distributed
  to agents. Each agent verifies the presented cert against the CA
  **offline** and authorizes from the roles in the cert — no Auth Service
  round-trip on the hot path; agents are stateless. Existing certs keep
  working if the Auth Service is down; only issuance/renewal needs it.
- **Revocation — the sharpest articulation of the late-converging
  problem:** primary story is **TTL** (short-lived certs), supplemented by
  **Locks** (lockable: user, role, MFA device, node UUID, join token...).
  Locks are resources on the Auth backend; agents run lock watchers and
  terminate matching sessions. When lock data is stale/unsynced,
  per-cluster (and per-role overridable) **lock mode**: `strict` = fail
  closed ("all interactions terminated when locks are not guaranteed up to
  date") vs `best_effort` (default) = keep enforcing last-known locks. One
  `strict` role suffices to make the local view strict.
- **Portability verdict:** Highly portable — capability-in-credential
  verified locally against a replicated root, TTL as the revocation floor,
  replicated revocation rows on top, and an *explicit named policy* for
  stale-authority behavior.

## 7. Fly.io — Corrosion internally

- **Write scoping is convention, not mechanism.** "Workers own their own
  state" — each physical server is source of truth for its own Fly Machines
  and only publishes rows about its own workloads, so "updates from
  different workers almost never conflict." No ACL, no enforcement;
  discipline in flyd's code. They later moved to re-publishing a Machine's
  entire row-set per change to avoid partial-state bugs.
- **Substrate:** SWIM gossip + cr-sqlite LWW (causal timestamps), QUIC
  transport, all inside a global WireGuard mesh — network membership is the
  only gate. Corrosion itself supports gossip TLS/mTLS (`gossip.tls`,
  `gossip.tls.client`) and an API bearer token (`api.authz.bearer-token`),
  but **nothing scopes which peer may author which rows — any gossip peer
  can write anything**.
- **The tail risk, demonstrated:** Fly infra-log 2024-09-07 — one poisonous
  configuration update was gossiped fleetwide in seconds and deadlocked
  essentially every fly-proxy on the platform (~40 min acute outage).
  Mitigations were **rate limits and regionalized Corrosion clusters — not
  authorization**.
- **Verdict:** Proves convention-only scoping is livable day-to-day and
  that unscoped gossip gives any single writer fleetwide blast radius; the
  operator of both endpoints (Fly owns all workers) is what makes
  convention tenable for them.

## 8. Uncloud (verified from source, github.com/psviderski/uncloud)

- **No authority model exists beyond WireGuard membership.** Verified in
  `internal/machine/machine.go` +
  `internal/machine/corroservice/config.go`:
  - Corrosion gossip runs **`Plaintext: true`** on the WireGuard mesh
    management address (default port 51001) — WireGuard *is* the entire
    trust boundary; any machine in the mesh can author any change to any
    row of cluster state.
  - The Corrosion HTTP API binds **127.0.0.1:51002** with a per-machine
    random 16-byte bearer token generated at init — this guards against
    *other local processes*, not other machines.
  - Join = CLI SSHes in, installs `uncloudd`, exchanges WireGuard keys with
    one existing machine; everyone else learns the peer from replicated
    state and auto-establishes tunnels. Operator access = SSH to any
    machine; all machines are equivalent, "every machine can control
    everything."
- **Verdict:** The closest comparable ships exactly the
  membership-is-full-authority model and simply ignores the tiering
  problem — evidence that solving it would be first in this niche, and that
  Corrosion's own config surface (mTLS gossip, bearer API) tops out at
  membership-grade auth.

---

## Shortlist: patterns that genuinely work WITHOUT a central runtime

1. **Signed authority statements replicated as ordinary data, verified
   locally** — trust rooted in signing keys, the distribution channel
   untrusted; revocation is a cosigned record; unsynced nodes honor stale
   authority until convergence, with a majority-fork rule bounding damage.
   *Proved by Tailscale tailnet lock (TKA).* Maps directly onto Corrosion
   rows: tier grants are rows signed by tier-granting keys; each machine
   verifies signatures against its local replica before honoring a caller.
2. **Capabilities/roles carried in a locally verifiable credential, TTL as
   the revocation floor** — verify offline against a replicated root;
   issuance can be rare/offline (a ceremony, not a runtime service); layer
   replicated revocation rows on top with an explicit stale-mode. *Proved
   by Teleport (roles-in-cert + Locks).*
3. **Caller-identity == row-owner as a pure function, applied at every
   acceptance point** — no policy objects at all; the identity convention
   plus one rule confines each machine to its own testimony. *Proved by
   Kubernetes NodeRestriction* — with the explicit caveat that kube gets to
   enforce it once because etcd hides behind one door; a coreless port must
   enforce at **every machine's HTTP door and at CRDT merge/gossip
   acceptance**, or it's decorative (Fly's outage is the demonstration).
4. **Identity → key-prefix write ranges as the policy schema** — the
   minimal expressible scoping vocabulary if scoping ever needs to be data
   rather than a hardcoded rule. *Proved by etcd RBAC* (policy replicated
   with the data, every replica enforces).
5. **Enforce from the last-replicated policy, with a *named* staleness
   policy** — receiver-side enforcement keeps working when whatever
   authored the policy is gone; the design decision is not "does revocation
   lag" (it always does) but "which mode: extend-cache / best_effort vs
   strict/fail-closed." *Proved by Tailscale packet filters, Consul
   `acl.down_policy`, Teleport lock modes.*
6. **Membership key as the outermost gate only** — decentralized,
   rotation-friendly, and structurally tier-blind; per-node revocation =
   full rotation. *Proved (as both floor and ceiling) by Serf/memberlist —
   and by Uncloud shipping nothing else.*

**Not portable (secretly central):** bearer tokens resolved against an
online store (Consul); a single API-server choke point guarding the store
(Kubernetes); convention-only write discipline with unscoped gossip (Fly,
Uncloud — works until the one bad write).

**The coreless-specific trap surfaced by this survey:** every enforcement
story that ports assumes checks happen *where changes are accepted*, not
just where requests arrive. In a CRDT/gossip store there are two acceptance
points per machine — the local HTTP door and the merge of remote changesets
— and Corrosion today authenticates peers (mTLS at best) but cannot
attribute or scope *rows* to *writers*. Tailnet lock is the existence proof
that per-row/per-key signatures verified at merge time close that gap
without any runtime service.

Sources: [Tailnet lock KB](https://tailscale.com/kb/1226/tailnet-lock),
[Tailscale ACLs](https://tailscale.com/kb/1018/acls),
[Tailscale grants](https://tailscale.com/kb/1324/grants),
[How Tailscale works](https://tailscale.com/blog/how-tailscale-works),
[Consul ACL overview](https://developer.hashicorp.com/consul/docs/secure/acl),
[Consul ACL agent config](https://developer.hashicorp.com/consul/docs/reference/agent/configuration-file/acl),
[Consul keyring rotation](https://developer.hashicorp.com/consul/docs/secure/encryption/gossip/rotate/vm),
[Kubernetes Node authorization](https://kubernetes.io/docs/reference/access-authn-authz/node/),
[Nomad gossip encryption](https://developer.hashicorp.com/nomad/docs/secure/traffic/gossip-encryption),
[Nomad security model](https://developer.hashicorp.com/nomad/docs/architecture/security),
[etcd RBAC](https://etcd.io/docs/v3.6/op-guide/authentication/rbac/),
[Teleport core concepts](https://goteleport.com/docs/core-concepts/),
[Teleport locking](https://goteleport.com/docs/identity-governance/locking/),
[Fly.io Corrosion blog](https://fly.io/blog/corrosion/),
[Fly infra-log 2024-09-07](https://fly.io/infra-log/2024-09-07/),
[Corrosion gossip config](https://superfly.github.io/corrosion/config/gossip.html),
[Corrosion API config](https://superfly.github.io/corrosion/config/api.html),
[Uncloud repo](https://github.com/psviderski/uncloud)
(source inspected: `internal/machine/machine.go`,
`internal/machine/corroservice/config.go`).
