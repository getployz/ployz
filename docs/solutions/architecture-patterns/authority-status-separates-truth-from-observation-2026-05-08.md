---
title: Authority Status Separates Truth From Observation
date: 2026-05-08
category: docs/solutions/architecture-patterns/
module: authority status surfaces
problem_type: architecture_pattern
component: tooling
severity: medium
applies_when:
  - Building status surfaces from durable records plus live backend probes
  - Reporting control-plane ownership, data buckets, or loss impact
  - Handling probe failure without fabricating stored truth
  - Sharing status metadata between JSON APIs and plain CLI output
tags:
  - authority
  - status-surfaces
  - control-plane
  - nats
  - stored-truth
  - live-observation
  - asset-manifest
---

# Authority Status Separates Truth From Observation

## Context

The authority status slice added operator-facing vocabulary for node posture, NATS asset buckets, and control-plane loss impact. A review caught places where the first implementation blurred three different facts:

- durable authority truth from stored machine membership,
- static NATS asset metadata from the manifest,
- live observation from probe and replica health.

The fix turned this into a reusable rule for status surfaces: derive durable posture from stored truth, and attach live uncertainty to real objects without inventing replacement truth.

## Guidance

Derive local authority posture from the authoritative self-record. If the record is missing, omit the posture rather than falling back to runtime config.

Before, `ployz status` could report local authority posture from active config:

```rust
local_authority: Some(AuthorityNodePosture::from_storage_participation(
    net.storage,
    &net.storage_participation,
)),
```

After, status reads the self-record once and derives both lifecycle and posture from that stored membership truth:

```rust
let local_machine = active.mesh.authoritative_self_record().await;
let local_machine_lifecycle =
    local_machine.as_ref().map(|machine| machine.lifecycle);
let local_authority = local_machine
    .as_ref()
    .map(AuthorityNodePosture::from_machine_membership);
```

For NATS assets, build the static manifest without requiring a live client. On connection failure or timeout, return one `unknown` health row per real manifest asset with `data_bucket`, `loss_impact`, installation, authority, domain, and scope preserved.

Before, probe failure became a fake asset:

```rust
NatsAssetStatus {
    name: String::from("hub"),
    kind: String::from("connection"),
    data_bucket: ControlPlaneDataBucket::HealthMetrics,
    loss_impact: ControlPlaneLossImpact::Unknown,
    state: NatsAssetHealthState::Unknown { error },
}
```

After, the failure is attached to each real asset:

```rust
manifest
    .iter()
    .map(|asset| {
        nats_asset_status_for_scope(
            scope,
            asset,
            NatsAssetHealthState::Unknown {
                error: error.clone(),
            },
        )
    })
    .collect()
```

Keep public status enums limited to states the system can produce today. Future-looking variants such as unused node roles or unknown loss classes become API commitments as soon as they are serialized.

Use the same key vocabulary in structured JSON and parseable plain output. For authority posture, the stable plain keys are `authority_role`, `authority_data_bucket`, and `authority_loss_impact`.

## Why This Matters

Authority status is operator-facing truth. Falling back to config can make stale or incomplete local configuration look like persisted cluster fact. That hides the important failure mode: the daemon cannot prove its own authority record exists.

NATS probe failures are live observation failures, not a reason to erase static knowledge. Operators and agents still need to know whether the affected object is stored intent, a projection, or a live fact. A fake `hub` row removes that context and makes every NATS failure look equally ambiguous.

Unused public enum variants create false API promises. Downstream consumers may branch on variants the system never emits, and future docs/tests then have to explain impossible states.

## When to Apply

- A status or report endpoint combines persisted control-plane truth with live backend probes.
- A payload exposes ownership, lifecycle, data-bucket, or loss-impact metadata.
- A backend probe can fail before reading live details, but static object metadata is known locally.
- A new public enum variant is being added before any producer exists.
- A CLI plain renderer mirrors structured API facts for agents.

## Examples

`crates/ployzd/src/daemon/handlers/status.rs` now derives local posture from `active.mesh.authoritative_self_record()` and leaves `local_authority` empty when that record is missing.

`crates/ployz-types/src/model.rs` now exposes `AuthorityNodePosture::from_machine_membership`, so status and machine inventory share the same membership-to-posture mapping.

`crates/ployz-nats/src/lib.rs` now exposes `NatsStore::asset_manifest_for_scope`, allowing status code to build the expected manifest before a live NATS connection exists.

`crates/ployzd/src/daemon/handlers/status.rs` uses that manifest to return `NatsAssetHealthState::Unknown` rows for real assets on connect failure or timeout.

`crates/ployz-nats/src/buckets.rs` exhaustively asserts every current stream/KV asset's `kind`, `scope`, `data_bucket`, and `loss_impact`, rather than checking only representative examples.

`crates/ployzd/src/daemon/handlers/machine/tests.rs` covers authority storage, storage candidate, and compute rows so the same posture vocabulary stays correct across user-visible surfaces.

## Related

- `docs/authority-roadmap.md` defines authority as ownership, not geography, and introduces the data-bucket vocabulary.
- `docs/plans/2026-05-08-002-feat-authority-status-slice-plan.md` is the implementation plan this learning refines.
- `docs/plans/2026-05-08-001-feat-authority-roadmap-plan.md` is the broader roadmap execution plan.
- `docs/testing/behavior.md` already reinforces status surfaces that separate intent, stored status, and live observation.
- `docs/routing-and-deploys.md` applies the same bucket vocabulary to deploy truth and routing projections.
- `docs/nats.md` is the short NATS substrate reference and defers product semantics back to the authority roadmap.
