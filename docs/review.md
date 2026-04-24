# Review — `feat/managed-domain-certs`

> **Status update (post-fixes).** Most of the review items below have been
> addressed on-branch. Each item is annotated with its current status:
> **RESOLVED**, **OPEN**, or **DEFERRED**. The "landed shape" section below
> describes what's actually merged so this doc stays useful as documentation
> rather than a stale checklist.

## Overview

Single feature: ACME/Let's Encrypt HTTP-01 issuance for operator-declared HTTPS
hostnames, an HTTPS listener in the gateway with SNI-based cert selection, and
preview-time DNS advice.

The shape fits AGENTS.md well: explicit state machine, event-triggered
issuance (deploy commit → spawn), projection-based gateway, hourly renewal
ticker that flips wall-clock-driven state and lets the normal pipeline do the
work.

## Landed shape

- **Two-phase issuance.** `start_order` (hot-path: account + `new_order` +
  challenge persist) runs synchronously in deploy apply with errors surfaced
  as warnings. `finalize_order` (background: `set_ready` → `poll_ready` →
  `finalize` → `poll_certificate`) runs fire-and-forget post-commit and on
  every renewal tick.
- **State machine.** `Pending → Issuing` on `start_order` success (writes
  `order_url`). `Issuing → Active` on `finalize_order` success. `Issuing →
  Failed` on finalize failure. `Active → RenewalDue` via ticker when past
  threshold. Stuck `Issuing` > 24h → `Pending` via ticker.
- **Renewal.** `spawn_certificate_renewal_ticker` runs at startup and hourly
  (±10% jitter, `PLOYZ_CERT_RENEWAL_INTERVAL_SECS` override). Renewal
  threshold is `not_before + 2 * lifetime / 3`, parsed from the real leaf via
  `x509-parser` — works for both 90-day and 6-day certs without a hard-coded
  30-day window.
- **Cluster-wide issuance lock.** `ResourceKey::CertIssuance(hostname)` +
  `OverlayIssuanceCoordinator` fan out `CoordOp::Prepare` to all active peers
  in parallel. Unreachable peers abstain; explicit `COORDINATION_DENIED` from
  any reachable peer vetoes this pass. Connection-bound lifetime — no TTL
  surgery.
- **Cluster-wide HTTP-01 readiness.** `Http01ChallengeReadiness` trait with
  `OverlayChallengeReadiness` impl that fans out a new `AcmeChallengeReady`
  RPC. Each peer waits up to 15s for the challenge row to replicate into its
  own store before confirming. `finalize_order` calls this before
  `set_ready`, so LE never probes a gateway that hasn't observed the token.
- **TLS callback logging.** `ManagedTlsCallbacks::certificate_callback` now
  logs every branch (missing SNI, hostname miss, PEM parse, chain install)
  with the hostname at `warn!`/`error!` level.
- **Hostname ownership admission.** `validate_hostname_ownership` reads
  committed routing state at preview/apply and rejects if another namespace
  already owns the hostname. Explicitly not fanout-backed — the race window
  across namespaces is narrow and damage is self-healing, and fanout doesn't
  actually close the window without blocking commits. Documented inline.

## High concern

- **~~`AuthoritativeResolver` runs on every preview AND every apply.~~**
  **RESOLVED.** The hand-rolled authoritative walker was removed entirely
  from `managed_domains.rs`. `warnings_for_plan` now only reports stored
  `CertificateState` — no DNS traffic on the control-plane critical path. If
  a DNS-advice UX is wanted later, wire it as a background observer that
  writes `DomainDnsAdvice` rows.

- **~~Certificate issuance has no mutex.~~** **RESOLVED.** `IssuanceCoordinator`
  trait in `ployz-orchestrator::certificates` with two impls:
  `NoopIssuanceCoordinator` for tests/single-process, and
  `OverlayIssuanceCoordinator` in `ployzd` for the real cluster fanout. Both
  the deploy path (`apply_with_certificate_coordination`) and the renewal
  ticker take a coordinator. Wire protocol extension:
  `ResourceKey::CertIssuance(String)` reuses the existing `CoordOp`
  machinery.

- **~~No renewal loop.~~** **RESOLVED.** `spawn_certificate_renewal_ticker` in
  daemon startup, skipped in memory-mode tests. Walks `Active` rows past
  `next_renewal_at`, flips to `RenewalDue`; walks stuck `Issuing` rows and
  flips to `Pending`; then drives `start_pending_orders`. Finalization is
  spawned separately so slow LE waits don't stall the ticker.

- **~~HTTP-01 validation race.~~** **RESOLVED.** `Http01ChallengeReadiness`
  trait gates `set_ready`. `OverlayChallengeReadiness` fans out the new
  `AcmeChallengeReady` RPC to every active peer; each peer confirms local
  store visibility within 15s or returns `ACME_CHALLENGE_NOT_READY`, which
  fails this finalization pass and bounces the cert back through reconcile.
  No more cold-gateway 403s on first cert.

- **~~`ManagedTlsCallbacks::certificate_callback` swallows all errors
  silently.~~** **RESOLVED.** Every `return` branch in `server.rs:29-105` now
  emits a `warn!` or `error!` with the hostname, including the underlying
  `error` where available. Missing SNI, hostname-not-found, PEM parse, key
  parse, and chain-install all log distinctly.

- **Private keys are replicated cluster-wide via Corrosion, unencrypted.**
  **OPEN.** `CertificateRecord.private_key_pem` still lands in every
  machine's corrosion data dir in plaintext. This is a deliberate
  shared-termination choice, but it should be recorded in VISION.md /
  AGENTS.md as an explicit property so a future reader doesn't mistake it
  for an oversight. At minimum, document: (a) what the trust boundary is
  for mesh members, (b) that data-dir backups contain all private keys
  forever, (c) what the recovery posture is for a compromised machine.

## Medium

- **~~Hardcoded 90-day cert lifetime.~~** **RESOLVED.** `finalize_one` parses
  `not_before`/`not_after` from the leaf via `x509-parser`.
  `renewal_threshold` uses `not_before + 2 * lifetime / 3`, which works
  correctly for both 90-day and the rolling 6-day short-lived certs. The
  90-day constant survives only as a parse-failure fallback.

- **~~Stuck `Issuing` rows never reap.~~** **RESOLVED.** `reconcile_renewals`
  resets any `Issuing` row where `updated_at` is older than
  `STUCK_ISSUING_MAX_AGE_SECS` (24h) to `Pending`, with a `last_error`
  breadcrumb. Matches the 7-day LE order expiry comfortably.

- **~~`list_certificates` + `list_acme_challenges` on every gateway snapshot
  reload.~~** **RESOLVED.** Gateway sync now subscribes to both tables via
  `subscribe_certificates` / `subscribe_acme_challenges` (same pattern as
  `subscribe_machines`), maintains authoritative in-memory caches, and
  applies per-row `Added`/`Updated`/`Removed` events incrementally. No
  full-table pull per invalidation; `SubscribeEvent` → one `HashMap` mutation
  → snapshot rebuild.

- **~~O(N) linear scans per SNI handshake / HTTP-01 request.~~** **RESOLVED.**
  `GatewaySnapshot.certificates` is now `HashMap<String, CertificateView>`
  (hostname → view) and `GatewaySnapshot.acme_challenges` is
  `HashMap<(String, String), AcmeChallengeView>` ((hostname, token) → view).
  `ManagedTlsCallbacks::certificate_callback` does an O(1) `get` by SNI name;
  `match_acme_challenge` does an O(1) `get` by (host, token). One fix fell
  out of the other — the incremental subscription cache is already keyed
  the right way.

- **~~`CertificateRecord.account_id` is always the literal
  `"letsencrypt-production"`.~~** **RESOLVED.** New
  `account_id_for_issuer_url(issuer_url)` helper in `certificates.rs` derives
  the ID from the issuer URL; every `CertificateRecord` construction site
  (`ensure_certificate_intents`, `load_or_create_account`, test fixtures)
  calls through it. Covered by `account_id_tracks_issuer_url` unit test.

- **Orchestrator build-graph cost.** **OPEN.** `ployz-orchestrator` now pulls
  in `instant-acme` and `x509-parser` directly (hickory-client was removed
  with `AuthoritativeResolver`). Still heavier than the inner-loop target
  set, though the net add is roughly zero vs. the starting branch.

## Testing

- **~~`AuthoritativeResolver` has no unit tests.~~** **N/A.** Resolver removed.
- **Eight cert-lifecycle unit tests in `certificates.rs`** covering
  `start_pending` success/failure/hostname-filtering, `finalize_due`
  success/failure-preserves-active, `renewal_threshold` math,
  `reconcile` flips Active→RenewalDue and resets stuck Issuing. Good
  coverage of the state machine.
- **~~`with_managed_tls`, `match_acme_challenge`, `ManagedTlsCallbacks`~~**
  **RESOLVED.** `routes.rs` has 13 direct tests covering `with_managed_tls`
  (active-only filter, missing-version-id rejection, hostname
  normalization, multi-challenge projection) and `match_acme_challenge`
  (happy-path, unknown host, wrong token, missing `/.well-known/` prefix,
  absent Host header, host-case / port normalization).
  `ManagedTlsCallbacks::certificate_callback` was refactored to delegate to
  a pure `resolve_tls_material(&GatewaySnapshot, Option<&str>) ->
  TlsResolution` helper; 6 unit tests exercise all 5 non-install branches
  (`MissingSni`, `HostnameMiss`, `FullchainParse`, `EmptyFullchain`,
  `PrivateKeyParse`) plus the `Ready` happy path (via `rcgen` self-signed
  cert, dev-dep). The pingora `ssl_use_*` install calls are still
  exercised only transitively by the metrics-listener test, which is the
  right cut — those are glue, not logic.
- **End-to-end ACME test** **OPEN.** No Pebble / LE-staging exercise. Path is
  manually tested only.

## Recommendation

**Ship.** The High blockers are all resolved on-branch. The remaining OPEN
items are either (a) documentation/scale ergonomics that don't block a first
release of this feature, or (b) gateway-side test gaps that are worth
filling but aren't correctness risks at current scale.

One thing worth doing before merging:

1. **Document the key-replication posture.** A paragraph in VISION.md or
   AGENTS.md that says "private keys live in every mesh member's corrosion
   data dir; trust boundary is mesh membership." This is the only
   outstanding High-level design property that isn't explicit. (Or implement
   envelope encryption — see the separate design discussion — but the
   documentation is the minimum.)

Everything else can follow up.
