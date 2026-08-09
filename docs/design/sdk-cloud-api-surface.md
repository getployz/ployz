# The SDK/Cloud API Surface Over the HTTP Layer

Design spec from the wayfinder ticket [Decide: the SDK/Cloud API surface over
the HTTP layer](https://github.com/getployz/ployz/issues/796). Sits over the
[binary/crate topology](binary-crate-topology.md) (#790, which fixed the SDK's
construction: ts-rs types + thin hand-written client, no OpenAPI) and the
[mesh-provider + Principal spec](mesh-provider-and-principal.md) (#787). A
fresh-context Codex CLI second-opinion pass (`gpt-5.6-sol`, high effort,
read-only) reviewed all six decisions; its amendments are folded in below.

**The SDK is a Node-server-only library.** Browsers can neither run the
transport (undici dispatchers) nor reach the mesh; Cloud's browser tier talks
to Cloud's own server functions, never to a cluster. The SDK's consumers are
server-side: Cloud, operator scripts, CI.

## Transport: the SDK is transport-dumb

The SDK speaks plain HTTP/JSON/SSE to a base URL — the cluster machine's
derived mesh IPv6 — and never owns a tunnel, holds a WG key, or knows which
mesh provider the cluster runs. Its transport config is an injected undici
`dispatcher` (with `proxy` as convenience sugar over it).

Portability comes from a sidecar: the `ployz` binary grows a mesh-proxy
subcommand running the userspace-WireGuard connector (gotatun + smoltcp, no
root, no TUN, no NET_ADMIN) and exposing a local proxy port. Destination
routing is derivation-free: every cluster's control-plane prefix is
`fd | sha256(cluster_id)`, so destination IPv6 → that cluster's tunnel, one
peer identity per joined cluster. A self-hosted Cloud on any PaaS is a Node
container plus this one static binary. Hosts with kernel WireGuard or
Tailscale skip the sidecar entirely; the SDK cannot tell the difference.

Sidecar boundaries:

- The proxy listener is an **operator credential**: it binds loopback or a
  Unix socket, never a cross-container address, since anything that can reach
  it acts with full `Peer` authority.
- The sidecar owns enrollment (redeeming a `pzjoin_` blob → `peers` row +
  stored WG identity) and key persistence. On ephemeral hosts the identity
  state must persist across restarts — a restart must not mint a new peer.
- Join blobs and enrollment material are redacted from logs and telemetry.

A rejected alternative: a napi-rs addon embedding the WG stack in Node —
per-platform native builds inside the client library and a provider weld.

## Identity: the SDK carries zero credentials

Authentication is entirely transport-level. Packets arriving over an enrolled
peer identity resolve source address → `peers` row → `Principal::Peer`; the
SDK holds no API keys, bearer tokens, or certs, and its config contains
nothing secret. Inventing an SDK auth header would be a second identity
system beside the Principal rule's "no side door, no ambient identity."

Credentials have moved, not disappeared: the sidecar's WG key *is* the
operator credential, and code execution on an enrolled peer host is full
operator authority — the trust ceiling #782 already accepted (membership =
write authority; admission is the security decision). The public API derives
identity from the actual mesh source address. A follower uses private
machine-authenticated headers for exactly one hop to the Preferred Controller;
peer-supplied copies are ignored.

## One machine per client; no silent failover

A client instance pins to one answering machine. The SDK never fails over
between machines behind the caller's back: a silent failover can land on a
machine with an older Corrosion view, making watched state visibly regress.
Moving machines is the caller's explicit act — construct a new client (the
roster is replicated, so any machine can name the others). The same rule
covers mutations: the answering machine forwards to the current Preferred
Controller, but **the SDK never auto-retries a mutation** because an accepted
in-memory controller attempt may disappear before its first coarse Operation
row is written. Auto-reconnect exists only for reads and streams. Retrying a
failed mutation is the caller's decision after re-reading cluster state.

## The endpoint catalog (v1 wrapped surface)

All DTO types generate from `core` regardless; the catalog is what the
hand-written client wraps and what Cloud may build against. CLI and SDK share
**one HTTP surface** — no SDK-only endpoints, no CLI side channels.

1. **Reads via watch lenses** — machines, services, containers/machine-status,
   ops: each one snapshot query + the same lens as SSE watch. Cloud never
   touches raw Corrosion subscriptions or SQL; the lens is the contract.
2. **The caller-composed operation** — `deploy`, returning an op handle;
   `ops list` and lookup from coarse summary rows;
   operation watch is invalidation plus full re-query, not event replay.
3. **Logs** — tail + follow for a service/container.
4. **Tokens** — create, **list** (default-live, `--all`-equivalent flag for
   expired — mirroring the CLI semantics), revoke. Redemption stays at the
   public door, outside the SDK.
5. **`GET /version`** — explicit catalog member (below).

Deliberately out of v1: status/doctor verdicts (CLI-computed presentation),
upgrade, repair journeys (rm/reset/refound, #798), volume ops, rebalance, machine rm, namespace
administration. Each joins the SDK when Cloud grows a real UI for it —
additive, no retrofit. The underlying endpoints exist for the CLI regardless;
"out" means no wrapped method and no compatibility promise to Cloud.

## Stream shapes and the re-attach contract

Stream re-attach semantics are absorbed by the SDK so
Cloud never implements gap logic. Every stream carries server keepalives; the
SDK owns bounded auto-reconnect with backoff and a typed `StreamDown`
terminal state distinct from graceful completion (the "no external I/O waits
forever" rule, client-side). All stream methods accept an `AbortSignal`.

- **Watch lenses are state, not events.** Each lens is an async iterable of
  current values. Internally: snapshot → follow → on disconnect or gap
  overflow (the ~500-change/10-min window), silent full re-query and resume.
  The snapshot returns a watermark and the follow begins strictly after it —
  a literal "snapshot then subscribe" has a race. Cloud renders whatever
  arrives and never sees deltas, cursors, or gaps.
- **Op progress is coarse state.** A Corrosion subscription is only an
  invalidation; every wake or reconnect re-queries the full Operation row.
  There are no event cursors, sequence numbers, replay gaps, or detailed
  evidence attachment. A live deploy that detects a foreign Controller
  Appointment ends as interrupted; a controller crash may leave a nonterminal
  evidence row and requires the caller to re-read reality before retrying.
- **Logs are a tail** — no replay guarantee. A reconnect that may have lost
  lines emits an explicit gap marker rather than splicing silently.

## `/version`: expose, don't enforce

`client.version()` wraps `GET /version → {major, build, features[]}` and the
SDK does nothing else with it: no auto-fetch on construction (constructing a
client costs zero round-trips), no SDK-side gating, no refuse-on-unknown-
major. Capability judgment lives in Cloud — the always-current caller that
carries all cross-version adaptation; down-adapters are built there at
major #2.

Refusals are authoritative for an attempted action, but they do not replace
capability discovery: Cloud shapes its UI from `features[]` (don't offer
what the cluster lacks) while tolerating the cluster refusing anyway. The
response describes **the answering machine**, honest under mixed-version
rollout. Feature names are namespaced, additive, and never reused with new
semantics; `build` is opaque diagnostic data. Feature strings type as
`KnownFeature | (string & {})` — known values compiler-checked, unknown
values tolerated per the additive law.

## The error contract: Results, not throws

Every method returns plain serializable data, structurally identical to
better-result's `SerializedResult` — the RPC-boundary shape Cloud's chosen
Result library rehydrates with one native `Result.deserialize` call:

```ts
type SdkResult<T> =
  | { status: "ok";    value: T }
  | { status: "error"; error: PloyzError };
```

- **`PloyzError` is one `core`-generated, kind-tagged union**: the cluster's
  typed refusals (additive within a major) plus the client-side failure
  classes — transport unreachable, timeout/abort, malformed
  response/protocol — kept as distinct kinds, because invalid JSON is not a
  transport failure and Cloud renders them differently.
- **Unknown refusal kinds have a runtime shape**: a variant the generated
  union cannot name normalizes to `{kind: "unknown", originalKind, value}`
  — a closed TS union alone does not make an unknown JSON variant safe.
  Tolerant reads are enforced in code, not assumed in types.
- **No throwing anywhere in the core client.** Fetch and decode failures are
  caught inside methods; throwing is reserved for bugs. Streams surface the
  same union as typed terminal events — no separate SSE error vocabulary.
- **One exported `unwrap(result)` helper** (~5 lines, generic over every
  method because the error channel is uniform) throws a `PloyzApiError`
  carrying the structured error and cause, for consumers wanting
  rejected-promise semantics: `useQuery({ queryFn: () =>
  sdk.machines.list().then(unwrap) })`.
- **Branded id types** in `generated.ts` extend the typed-ids law across the
  language boundary at zero runtime cost.
- The wire tag stays product-owned (`kind`, the serde tag) — the Rust
  contract does not adopt a JS library's field convention.

Cloud-side synergy, recorded not mandated: the SDK's return shape is the
same shape Cloud's `serializeHandler` emits, so server functions pass SDK
results through with redaction rather than re-wrapping.

Named upgrade path: a types-only generated endpoint manifest driving one
generic client function (the tRPC-flavored shape) replaces the hand-written
methods if the catalog grows past a couple dozen entries; at the v1 size the
generated SDK diff already tripwires drift.

## Considered and rejected

- **Throwing SDK (Stainless/Stripe shape)** in all variants — raw + adapter
  module, uniform-error + generic wrapper, matchError-compatible error class,
  dual throwing/Result surface: every variant keeps `throw` as the SDK↔Cloud
  channel, which TypeScript cannot type, forcing re-assertion on the far
  side; Cloud already converts every throwing SDK it touches into Results at
  the boundary.
- **SDK depends on better-result**: welds the public API to a third-party
  major and its dual-package `instanceof` hazards; structural compatibility
  buys the same ergonomics dependency-free.
- **AWS Smithy-style command codegen**: ceremony built for a 300-service
  surface, applied to ~12 methods.
- **tRPC/Hono route-type inference**: requires a TS server; ours is Rust
  (the manifest middle path is the named upgrade above).
- **openapi-fetch**: leans on an OpenAPI document #790 already declined to
  maintain.
- **SDK-shipped TanStack Query `queryOptions`**: wrong layer — only Cloud's
  server tier has mesh reachability, so query factories belong in Cloud's
  models layer behind server functions.
- **Runtime schema validation (zod/valibot)**: generated strict validators
  break the tolerant-read rule the cross-version policy stands on.
