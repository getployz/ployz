# PR #98 — Add ZFS support across the stack

**Scope:** 143 files, +17,043 / −1,246 lines. Despite the title, this PR bundles four largely independent initiatives: ZFS volumes + transfer, ACME/certificates, gateway HTTPS/managed-TLS, and DNS resolver rework.

## Top-level recommendation: split this PR

The ZFS surface, ACME/certificates surface, and gateway TLS/DNS surface are mostly orthogonal — none of the cert code references ZFS, and the DNS rework is independent of both. A single 17k-line PR makes safe review and rollback impossible. At minimum, separate (1) ZFS + transfer, (2) ACME + cert coordination + managed_domains + gateway TLS, (3) DNS resolver rework. The previous version of this file was a self-review of an earlier, ZFS-only iteration of the branch (PR #83); it's now stale and should not be checked in long-term.

## Repo hygiene (must fix before merge)

- **`.DS_Store` is committed** at the repo root. Add to `.gitignore` and delete the blob.
- **`docs/review.md`** — this file. Either remove or move out of the repo before merge.
- **Pebble fixtures** (`packaging/e2e/pebble/pebble-server.key`, `pebble.minica.pem`) — verified test-only (Pebble's documented test CA), safe.

## Pre-merge correctness fixes

### ZFS transfer (network-facing, security-sensitive)

- **`transfer_listener.rs:201`** — header size check is off-by-one: `read_until` with `take(MAX_HEADER_BYTES + 1)` then `header.len() > MAX_HEADER_BYTES` admits a buffer that's exactly `MAX_HEADER_BYTES + 1` bytes (header + trailing `\n`). Tighten to `>=` or strip the delimiter before measuring.
- **`zfs.rs:924-930`** — `tokio::io::copy` failure path may leak the `zfs send` child: if `kill()` also fails, `wait_with_output()` is skipped. Always reap (`.wait()` in a defer-style guard) to avoid zombies on stream errors.
- **`zfs.rs:434`** — `declared_quota_bytes_excluding` uses `.unwrap_or(0)` on quota parse. A malformed stored quota silently becomes "no quota," which can break the shrink-rejection guard. Surface the error or fail closed.
- **`transfer_listener.rs:144`** — `validate_open_source` checks overlay IP before checking volume ownership; ownership-check error responses then differ by namespace existence, leaking which volumes exist on the receiver to any peer on the overlay. Reorder, or return identical errors regardless of cause.
- **`transfer_listener.rs:163-167`** — GUID-mismatch response echoes the receiver's local GUID back to the sender. That's snapshot metadata leaking to a peer that just failed an auth check. Send a generic error.
- **`zfs.rs:257`** — `unique_transfer_id` does `SystemTime::now().unwrap_or_default()`, which makes nanos = 0 on clock failure and weakens uniqueness. Per AGENTS.md, no `unwrap_or_default()` on state.

### Defensive Rust (AGENTS.md violations)

- **`cert_coordination.rs:509`** — `.expect("release fanout should not serialize peers")` on a recoverable serialization error; convert to `Err`.
- **`certificates.rs:609`** — order resumption calls `resume_order(order_url)` without validating the URL's origin against the configured ACME directory. A corrupted `CertificateRecord.order_url` would let a finalize step talk to an attacker URL. Validate that the host matches the directory's host before resume.

### Layering / architecture

- **`ployz-orchestrator/src/certificates.rs` (1789 lines)** — concrete ACME backend (`InstantAcmeIssuer`) lives in the orchestrator crate, which AGENTS.md scopes to "orchestrator core, kernel, contracts." The pattern elsewhere in this repo (mesh drivers, store backends) is: trait in core, concrete impl in a backend crate. Extract the issuer + HTTP-01 polling + Pebble glue to a sibling crate (e.g. `ployz-cert-backends`); keep only the coordination types and traits in orchestrator.
- **Reachability rework** — `mesh/probe.rs` (332) and `machine_reachability.rs` (136) deleted; replaced by `deploy/probe.rs` reading cached `PeerMembershipObservation` rows. Per AGENTS.md, "reachability checks belong at decision time through direct probes, not freshness timestamps." Cached membership is freshness-derived. Acceptable if intentional, but worth flagging in the PR description: deploys will now succeed-then-fail-late on machines that died between gossip ticks. Confirm this is the intended trade.

## Real but lower priority

- **`gateway/server.rs:81-150`** — TLS certificate callback clones `X509` and `Vec<u8>` chain on every handshake (`leaf.clone()`, `chain.to_vec()`). High-RPS sites will feel this. Hold leaf/chain as `Arc<X509>` / `Arc<[X509]>` in the snapshot.
- **`gateway/sync.rs:60-96`** — `ManagedTlsCache` (`HashMap<String, CertificateRecord>` + challenge map) has no upper bound. If projection ever loops or a buggy upstream keeps generating challenges, it grows unbounded. Cap or assert size at projection time.
- **`gateway/server.rs:46-86`** — no timeout around `X509::stack_from_pem` / `PKey::private_key_from_pem` in the TLS callback. Validate PEM at projection time so handshake-path callbacks can't stall on malformed cert material.
- **`dns/server.rs:70-75`** — IPv6 clients are silently treated as having no caller namespace, so bare-name lookups never resolve. Either document explicitly or warn at first IPv6 query.
- **`dns/resolve.rs:88-175`** — `parse_query` falls through to `DnsQuery::Unknown` rather than exhaustively matching enum variants; AGENTS.md disallows wildcard-style fallthroughs on project-defined enums. Spell out variants.
- **`ployz-corrosion/.../acme_challenges.rs`** + **`certificates.rs:782`** — ACME `key_authorization` and the full `AccountCredentials` (private key) are stored as plaintext JSON in Corrosion. This is acceptable *only* under the assumption that Corrosion replication never leaves the WireGuard overlay and nobody backs up the SQLite file unencrypted. Worth a `// SECURITY:` comment at the column definition documenting the assumption.
- **`deploy/managed_domains.rs:69-70`** — acknowledged hostname-collision race across namespaces. Fine for v1, but the comment should turn into a tracked issue, not a code comment that quietly rots.
- **`deploy/plan.rs:551`** — `volume_record_needs_update() -> bool`; AGENTS.md prefers enums (`Create | Update | Skip`) for multi-state decisions. Minor.
- **Volume lifecycle gap (carried over from prior review)** — services removed from a manifest while their volume is retained leave `attached_services` pointing at deleted services. No test covers this; please add or document.
- **Quota parser duplication** — `parse_size_bytes` (storage/zfs.rs) and `quota_value` (deploy/plan.rs) still diverge on return type and fallthrough. Lift to `ployz-types` next to `VolumeDeclaration`.

## Test coverage gaps

- No test for ACME challenge visibility timeout / poll-interval behavior (`certificates.rs:696-722`).
- No test for malformed `account_credentials_json` rehydration.
- No test for the volume-without-attached-service lifecycle described above.
- No test for concurrent cross-namespace hostname race (`managed_domains.rs`).
- No test for quota parser overflow (`16T`+) or a single canonical parser.
- No test for `start_candidate` returning a clear error when a service uses a managed volume but `storage.zfs_root` is unconfigured (the `service uses managed volumes but daemon has no [storage] zfs_root configured` branch in `local.rs`).

## What's solid

- ZFS adoption matrix (`storage/zfs.rs`) — idempotent ensure, fake-shell-runner coverage of create / adopt / grow / shrink-refuse / mountpoint-mismatch.
- Cert coordination's lock-bound re-read pattern (`certificates.rs:349-357`) correctly closes the duplicate-order race.
- New `managed_domains` module is a clean separation; doesn't leak into cert coordination.
- Store-API additions (`list_volumes` / `get_volume`) are domain-neutral; no ZFS concepts leak upward to the trait layer.
- Slice patterns and `#[must_use]` are applied consistently in new code; no `.unwrap()` on `Option` state in non-test paths (apart from the items called out above).
- Manifest restructure (`VolumeMount`/`VolumeSource` → `Mount`/`MountSource`, `MountSource::Volume(String)` as a reference, top-level `VolumeDeclaration` + `VolumeScope`) holds up well under planner integration.

## Tally

- Repo hygiene: 2 must-fix (`.DS_Store`, `docs/review.md`).
- Correctness pre-merge: 7 (ZFS transfer 6, cert URL validation 1).
- Architecture: 2 (cert backend layering, reachability-via-cache trade).
- Lower priority: 9.
- Test gaps: 6.

Headline ask: **split the PR** before deeper iteration. The ZFS-only slice is closest to mergeable; the cert/gateway slice needs the layering rework and the cache/clone fixes; the DNS slice is small enough to land on its own.
