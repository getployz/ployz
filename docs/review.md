# Review — `feat/managed-domain-certs` (PR #74)

## Overview

Large, well-structured feature landing managed TLS via ACME HTTP-01: a
`certificates` table with a lifecycle state machine, an orchestrator
issuance/renewal engine (`instant-acme`), cluster-wide coordination
(per-hostname + per-issuer-account locks), HTTP-01 projection into the gateway,
and a new BoringSSL-backed HTTPS listener with SNI-driven cert selection.
Includes Pebble-based e2e coverage.

The shape matches the project's stated intent: explicit imperative transitions,
projection-based gateway, event-driven reconciliation with a single observation
ticker.

## Strengths

- **State-machine discipline.** `CertificateState` transitions are narrow and
  explicit; `installed_version()` decouples "what TLS can serve" from `state`
  so renewal (`Active → RenewalDue → Issuing`) never blackholes the old leaf.
  Good tests pin this.
- **Clustered order safety.** `start_one` acquires the cluster lock, re-reads
  under the lock, releases *after* the upsert. The
  `start_one_holds_lock_until_after_upsert` test codifies the contract. Matches
  the project's "no CRDT-CAS" reality.
- **Stale-finalize guard.** `finalize_one` re-reads and compares
  `(state == Issuing && order_url == expected)` before writing — prevents a
  slow background finalizer from stomping a newer order / active cert. Tests
  cover both stale-success-vs-new-order and stale-failure-vs-active.
- **Challenge pruning.** `prune_acme_challenges_for` runs under the lock before
  every new order, bounding `acme_challenges` growth across repeated failures.
- **Readiness fanout.** `OverlayChallengeReadiness` waits for the challenge row
  to replicate to reachable peers before `set_ready` — avoids the cold-gateway
  403 on first issuance.
- **Gateway projection.** `HashMap`-keyed `certificates` / `acme_challenges`
  are O(1) on the TLS + HTTP-01 hot paths. Subscription-based cache rebuilds
  avoid full-table pulls per invalidation.
- **Rich test coverage.** The `certificates.rs` and `routes.rs` suites document
  the tricky invariants (renewal still serves old leaf, stale writes,
  normalization, multi-daemon retries).

## Issues — worth addressing before merge

- **`rebuild_snapshot` has an unused generic parameter.**
  `ployz-gateway/src/sync.rs`:

  ```rust
  fn rebuild_snapshot<S>(state: RoutingState, cache: &ManagedTlsCache) -> ...
  where S: RoutingStore { ... }
  ```

  Nothing in the body references `S`. Drop the `<S>` / `where` clause; callers
  are calling `rebuild_snapshot::<S>` to satisfy a phantom bound.

- **`IssuanceHold::Drop` spawns the releaser on whatever runtime is live.**
  `certificates.rs`. If a deploy aborts and the runtime is already tearing
  down (e.g. SIGTERM mid-apply), `tokio::spawn` silently drops the release.
  The peer-side TTL (`DEFAULT_ISSUANCE_TTL_SECS = 300`) bounds the damage, so
  this isn't a correctness bug, but it's worth either (a) a `warn!` when the
  spawn fails, or (b) making `release().await` the only supported path and
  leaving `Drop` purely defensive with a warning. As-is, the abstraction
  invites "I'll just drop it" and silently stretches the lock for 5 minutes
  cluster-wide.

- **`release_peers` is sequential.**
  `cert_coordination.rs`. The happy path issues a release to every peer in
  series — contrast with `prepare_peer` which uses `join_all`. At ~100ms per
  peer over the overlay this is noticeable, and a slow/unreachable peer
  serializes the rest. Parallelize with `join_all`; failures are already
  swallowed with a `warn!`.

- **`AcmeAccountRecord.account_key_pem` is JSON, not PEM.**
  `certificates.rs` serializes `AccountCredentials` with
  `serde_json::to_string`, and the field is named `account_key_pem`. The name
  is a trap for any future reader who tries to parse it as PEM. Rename to
  `account_credentials_json` (or similar). Since this is a brand-new
  replicated table in this PR, rename now rather than carry it.

- **`RequestCtx::downstream_scheme` defaults to `""`.**
  `proxy.rs`. `request_filter` sets it before any other hook reads it, so
  today it's fine — but `#[derive(Default)]` on `&'static str` gives `""`,
  and a future hook that fires before `request_filter` would send
  `X-Forwarded-Proto: ` downstream. One-line fix: give it a `Default` impl
  that returns `"http"`, or hard-code the default in `new_ctx`.

- **Hardcoded e2e hostname `acme-smoke.test` only verified through
  `--resolve 127.0.0.1`.** `scenarios/deploy_smoke.rs` is a useful smoke but
  doesn't exercise actual DNS rebinding, so any future regression in the
  overlay-DNS + ACME interaction won't be caught here. Not a blocker, worth
  noting as a follow-up.

- **Private keys replicated cluster-wide in plaintext.** Agree with the earlier
  self-review: documenting the trust boundary in VISION.md/AGENTS.md is the
  minimum before release. The design-intent note should live in a durable
  spot, not this review file.

## Minor

- `ployz-orchestrator/src/deploy/execute.rs` — `final_preview.warnings !=
  managed_warnings` compares semantically different warning vectors. When
  `final_preview.warnings` starts as `Vec::new()` and `managed_warnings` is
  also empty, the update-and-re-upsert is skipped, which is fine; if
  `warnings_for_plan` evolves to include non-TLS warnings, this equality check
  will silently double-apply. Consider comparing by set or just always
  overwriting.
- `certificates.rs::prune_acme_challenges_for` does a full
  `list_acme_challenges` + per-row `delete_acme_challenge` — O(N_cluster) on
  every `start_one`. Fine at current scale; a
  `delete_acme_challenges_for_hostname(hostname)` store op would be cleaner.
- `GATEWAY_LISTENER_WAIT_TIMEOUT: Duration = Duration::from_secs(60)` in the
  deploy_smoke e2e — racy if pingora is slow to bind on a cold CI; consider
  polling logs rather than TCP dial.
- `finalize_order` in `certificates.rs`: the per-token delete loop after
  issuance doesn't run if `poll_certificate` errors out mid-stream, because
  of `?` propagation. Stale challenges then get cleaned up next pass by
  `prune_acme_challenges_for`. That's fine, just worth a short comment so
  future readers don't see it as a leak.

## Test coverage

Strong for the cert state machine + gateway projection. The Pebble-based
`deploy_smoke` covers the golden path. Gaps:

- No negative ACME test (LE 429 / DNS misconfigured); warning surfaces are
  unit-tested only.
- `OverlayIssuanceCoordinator` and `OverlayChallengeReadiness` have no
  integration tests against a real 2-peer overlay with one reachable peer
  denying; only unit-level coverage via `NoopIssuanceCoordinator` /
  `VetoAccountCoordinator`.

## Security

- Plaintext key replication — noted, OPEN.
- No hostname allowlist: any operator who can call `deploy` can trigger ACME
  orders for any name in any service's `hostnames`. LE rate-limit defeat
  comes "free" via `Failed → Pending` retry loop capped only by
  `STUCK_ISSUING_MAX_AGE_SECS` (24h); worth confirming the retry cadence on
  repeated `rateLimited` can't walk the LE budget.
- `AcmeChallengeRecord.expires_at` is persisted but never enforced — dead
  rows only get cleaned up by `prune_acme_challenges_for` on the next order
  for that hostname. Low risk, worth a reap pass in the renewal ticker.

## Recommendation

**Approve with small follow-ups.** The architecturally risky items (cluster
race on order creation, stale finalizer overwrites, TLS projection cost,
renewal correctness across 6-day and 90-day certs) are all addressed with
tests that pin the invariants. Most remaining items are naming, parallelism,
and test-gap polish. The plaintext-key-replication posture needs a VISION.md
paragraph before this ships publicly.
