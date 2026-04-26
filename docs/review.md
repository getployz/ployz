# PR #83 — Add first-class ZFS volumes

Review of the PR against the working tree on `feat/zfs`. Diff: 1513 additions / 76 deletions across orchestrator, runtime backends, store, daemon, types, and CLI.

## Overview

Introduces top-level managed volumes in the deploy manifest backed by a per-machine ZFS driver. Volumes carry strict identity (name, scope, quota, mode, owner) and are pinned to a single machine; services that mount them inherit that pin. New `volumes` Corrosion table persists records through deploy commits. Renames `VolumeMount`/`VolumeSource` → `Mount`/`MountSource` and tightens the bind-mount escape hatch to the system namespace only.

The shape is good: durable intent in store, planner pins placement, executor realizes via shell-driven `zfs` calls. Coverage looks reasonable for v1.

## What's solid

- **`ShellRunner` + `ZfsDriver`** (`crates/ployz-runtime-backends/src/storage/{shell,zfs}.rs`): clean trait abstraction, idempotent `ensure` (list → diff → narrow mutation), comprehensive `FakeShellRunner` tests covering create, adopt, grow, shrink-refuse, mountpoint-mismatch.
- **Manifest restructure** (`crates/ployz-types/src/spec.rs`): `ManagedVolumeSpec` deleted, `VolumeMount`/`VolumeSource` renamed to `Mount`/`MountSource`, `MountSource::Volume(String)` is a reference, `VolumeDeclaration` + `VolumeScope { Single, Shared }` as new top-level types.
- **Validation rules**: scope=single + replicas>1 rejected; bind mounts allowed only in system namespace; `Placement::Global + MountSource::Volume` rejected; volume reference resolution + duplicate detection covered.
- **Planner** (`crates/ployz-orchestrator/src/deploy/plan.rs`): correctly pins services to bound machines, detects multi-volume cross-machine conflicts, validates existing-volume immutability, threads pinning into `desired_slots` machine selection.
- **Daemon config + wiring**: `[storage] zfs_root` lands in `DaemonConfig`/`DaemonOverrides`, `DaemonState::zfs_storage_driver()` constructs the driver, threaded through `runtime_profile` and into `LocalDeployRuntime`.
- **Corrosion `volumes` table**: PK `(namespace, volume_name)` + payload_json with `INSERT ... ON CONFLICT DO UPDATE` upsert is consistent with the existing table style. Read filter `payload_json <> ''` matches the publication-boundary contract.
- **`#[must_use]` correctly added** on `root_dataset()` and `root_mountpoint()`. Slice patterns used throughout. Explicit `MountSource::Bind(_) | MountSource::Tmpfs => {}` arms — no wildcard.

## Pre-merge fixes

### 1. `build_committed_volumes` writes dataset/mountpoint without `zfs_root` prefix

`crates/ployz-orchestrator/src/deploy/execute.rs:421-427`

```rust
.unwrap_or_else(|| format!("{}/{}", plan.namespace().0, planned.declaration.name));
```

For new volumes this produces `prod/data` and `/prod/data` — but the actual dataset is `tank/ployz/prod/data` and the actual mountpoint is `/tank/ployz/prod/data`. The orchestrator crate has no access to `zfs_root` (runtime-side config). The runtime ignores the stored values and recomputes from `driver.root_dataset()` / `driver.root_mountpoint()`, so this isn't currently load-bearing — but `ployz volumes ls` and any future debug surface would render wrong paths, and the fields lie about what's on disk.

**Recommendation**: drop `dataset` and `mountpoint` from `VolumeRecord`. They're deterministic from `(zfs_root, namespace, volume_name)` and `zfs_root` is per-machine config — the record shouldn't pretend to carry them. Alternative: round-trip realized paths from agent → store after `resolve_volumes` succeeds.

### 2. `build_binds` silently drops a Volume mount when `resolved` is missing the entry

`crates/ployz-runtime-backends/src/deploy/local.rs:1200-1205`

```rust
MountSource::Volume(volume) => {
    let Some(host) = resolved.get(volume) else {
        return None;
    };
    ...
}
```

`resolve_volumes` is contracted to populate every `Volume` mount. If it doesn't, the workload silently starts without that mount and there's no signal. Defensive-Rust rule from `AGENTS.md` applies: don't unwrap-or-return-None on state that should never be missing.

**Recommendation**: replace with `.expect("resolved during start")` or return `Err`.

### 3. Volume name not validated

`crates/ployz-types/src/spec.rs` — `VolumeDeclaration::validate` checks quota/mode/owner but not `name`. Names flow into a dataset path (`format!("{root}/{namespace}/{name}")`) and a mount string. A name containing `/`, `..`, or whitespace would escape its namespace dataset.

**Recommendation**: add a strict charset check (e.g. `[a-z0-9_-]+`) like Docker volume names.

## Real, lower-priority

### 4. Adoption skips on-disk mode/owner verification

`ZfsDriver::ensure` (`crates/ployz-runtime-backends/src/storage/zfs.rs:1534`) verifies mountpoint and reconciles quota, but never re-reads `mode`/`owner` of an existing dataset. Manifest-level mode/owner changes are already rejected by `validate_existing_volume`, so the manifest path can't drift — the gap is **out-of-band drift**: an operator (or another process) `chmod`s the dataset, the next deploy adopts, and on-disk state silently diverges from the record.

**Recommendation**: on adoption, read mode/owner via `stat` and either reject divergence or reapply. Lower priority because manifest-level changes are already locked, but worth fixing to keep the record honest about reality.

### 5. No removed-volume handling

`commit_deploy` accepts `volumes` but no `removed_volumes`. Drop a declaration from the manifest and the row stays forever, plus any ZFS dataset is orphaned.

**Recommendation**: at minimum a follow-up issue/TODO; ideally the planner emits a removal set the same way it does for services. Dataset destroy should require explicit operator confirmation.

### 6. `get_volume` swallows duplicates

`crates/ployz-corrosion/src/store/tables/volumes.rs:240-244`

```rust
let [row] = rows.as_slice() else {
    return Ok(None);
};
```

Returns `None` for both 0 rows and 2+ rows. PK guarantees ≤1, but if that ever breaks the silent `None` is harder to debug than `Err`.

### 7. Quota parser duplicated

`parse_size_bytes` in `storage/zfs.rs:1697` and `quota_value` in `deploy/plan.rs:740`. Same suffix scheme, different return types (u64 vs u128) and slightly different fallthrough semantics.

**Recommendation**: lift into `ployz-types` next to `VolumeDeclaration`.

### 8. CLI `-v` rejected outside system namespace

`crates/ployzd/src/request_builder.rs:2670` now rejects `-v` outside the system namespace. This is a behavior change for any user currently using `ployz deploy service ... -v src:dst`. Worth a release-note entry pointing at the manifest-volumes path.

### 9. `ZfsDriver::read_dataset` treats any non-zero exit as "absent"

`crates/ployz-runtime-backends/src/storage/zfs.rs:1579-1581`

```rust
if output.status != 0 {
    return Ok(None);
}
```

Masks permission denied, ZFS daemon down, pool offline, etc. — they all collapse to "create the dataset," which then fails for the underlying reason. Diagnostics will be confusing.

**Recommendation**: distinguish "dataset does not exist" via stderr inspection or a dedicated existence check.

### 10. Tests in `zfs.rs` use slice indexing

`calls[2][..4]` etc. — minor AGENTS.md style nit ("slice patterns over indexing"). Not load-bearing; clippy may flag if `indexing_slicing` is enabled in tests.

## Acceptable v1 tradeoffs (won't fix)

Listed so future reviewers don't re-raise:

- **`volumes_json` sent per service** (`StartCandidateRequest`/`StartCandidate` frame). Redundant copies for N-service deploys, but small and not on hot path. Send-once-per-session cleanup later.
- **Resolve-per-replica for `Shared`**: a 5-replica `shared` volume calls `zfs list` 5 times, idempotent. Pre-resolve in planner if it shows up in deploy latency.
- **Quota shrink doubly-rejected**: planner rejects all shrinks via byte compare; driver also has a used-bytes guard. Stricter than necessary. Document as known limitation.
- **Manifest-level mode/owner changes rejected outright**: stricter than the original plan ("set on creation only"). Operators must nuke and recreate to change ownership. Add a migration verb in v2.

## Investigated, not an issue

- **`new_volume_machine` "fallback to `local_machine_id`" looked unsafe**: `plan.rs:783-787` has an `unwrap_or_else(|| local_machine_id.clone())` that appears to bind a volume to a non-deployable machine if `desired_machines` is empty. Verified `deployable_machines()` (`plan.rs:404-406`) returns `vec![local_machine_id.clone()]` when `enabled.is_empty()`, so `desired_machines` is never empty by the time `new_volume_machine` runs. The fallback is dead code in practice. The `machine_is_deployable` helper at `plan.rs:633-637` also explicitly treats local-not-in-`machine_map` as deployable, which keeps single-node bootstrap working.

## Test gaps

- No test for `start_candidate` returning a clear error when a service uses a managed volume but `storage.zfs_root` is unconfigured (the `service uses managed volumes but daemon has no [storage] zfs_root configured` branch in `local.rs:1171`).
- No test exercising `valid_quota`/`valid_mode`/`valid_owner` rejection paths individually.
- No test for volume-name validation (after fix #3 lands).
- No test for the ZFS driver's "child datasets in `zfs list` output" failure mode — currently `read_dataset` would error out, fine for now but worth a comment that `-r` is intentionally not used.

## Test plan readiness

Per the original plan's verification section, key end-to-end scenarios needing manual validation on a Linux box with a real zpool:

- Single-node smoke (single replica, single volume) — dataset created, chowned, mounted.
- Restart adoption — dataset reused, no recreate.
- Quota grow → applied; quota shrink with overflow → rejected.
- Sticky placement — drained bound machine causes loud failure, not silent re-placement.
- Detach + reattach across services — data preserved, machine pin propagates.
- Multi-attach (Shared) — two services pinned to same machine, mount same dataset.
- Multi-replica (Shared) — `replicas: 3` all on one machine, all mount same dataset.
- Single-scope rejection — `replicas: 2` + `scope: single` → manifest rejected.
- Re-declaration with changed scope — rejected.

Unit/integration tests covered in `just test` / `just test-all` should already exercise the validation/planner logic. The end-to-end flow with real ZFS is the remaining gap before launch.

## Suggested follow-ups (not blockers)

1. Round-trip realized `dataset`/`mountpoint` from agent → store after `resolve_volumes`, or drop the fields.
2. Add `valid_volume_name` and reject path-escaping characters.
3. Reconcile mode/owner on adoption (read on-disk via `stat`), or reject divergence.
4. Add removed-volume planning so dropping a declaration cleans up the row (and eventually the dataset, behind an explicit destroy).
5. Consolidate the two quota parsers.

## Tally

- 3 pre-merge fixes (1–3).
- 7 real lower-priority items (4–10).
- 4 acceptable v1 tradeoffs (won't-fix list).
- 1 investigated-and-cleared (`new_volume_machine` fallback).

Overall: solid v1 surface, conservative defaults (single-machine pin, immutable scope/mode/owner, no shrink), good unit coverage of the ZFS adoption matrix. The data-shape mismatch on `VolumeRecord.{dataset,mountpoint}` is the one I'd resolve before merging.
