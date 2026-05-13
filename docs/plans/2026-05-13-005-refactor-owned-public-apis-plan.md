---
title: "refactor: Remove Borrowed Data from Public APIs"
type: refactor
status: active
date: 2026-05-13
---

# refactor: Remove Borrowed Data from Public APIs

## Summary

Clean up public Rust API surfaces that expose caller-visible lifetimes for parsed data, configuration structs, and trait methods. Public APIs should accept borrowed inputs where useful and return owned or otherwise lifetime-free contracts unless the borrow is the actual purpose of the API.

## Requirements

- R1. Parsed data returned from public functions must be owned unless zero-copy behavior is explicitly part of the contract.
- R2. Public structs used across crate boundaries must not expose lifetime parameters for ordinary config/context data.
- R3. Borrowed helper functions that only serve an implementation crate must be lowered to crate/private visibility.
- R4. Async trait method signatures must use elided borrows or owned/static return aliases rather than named public lifetimes when the method does not return borrowed domain data.
- R5. Existing behavior and tests must continue to pass.

## Scope Boundaries

- Do not redesign subsystem ownership beyond the minimum API-shape cleanup.
- Do not add compatibility shims for the old borrowed types.
- Do not change wire formats, daemon behavior, image transfer behavior, routing behavior, or deploy semantics.

## Implementation Units

### U1. Own Docker Image Reference Parsing

**Goal:** Make Docker image reference parsing return owned parsed data.

**Requirements:** R1

**Dependencies:** None

**Files:**
- Modify: `crates/ployz-runtime-docker/src/runtime/image_ref.rs`
- Modify: `crates/ployz-runtime-docker/src/runtime/engine.rs`
- Modify: `crates/ployz-wireguard-backends/src/mesh/wireguard/docker/exec.rs`

**Approach:** Convert `DockerImageRef` fields to `String`/`Option<String>` and update callers/tests to borrow from the owned result only inside implementation logic.

**Test scenarios:** Existing image-ref parser tests must still cover untagged refs, tagged refs, registry ports, digest refs, and invalid digest rejection.

**Verification:** `cargo test -p ployz-runtime-docker --no-default-features image_ref` and compile checks for Docker/WireGuard callers.

### U2. Lower Internal Borrowed Helpers

**Goal:** Remove externally visible lifetimes from helper APIs that are only used inside their defining crate.

**Requirements:** R3

**Dependencies:** None

**Files:**
- Modify: `crates/ployz-orchestrator/src/machine_policy.rs`
- Modify: `crates/ployz-gateway/src/routes.rs`
- Modify: `crates/ployz-gateway/src/server.rs`
- Modify: `crates/ployz-runtime-docker/src/runtime/labels.rs`

**Approach:** Make route matchers and coordination helpers private or crate-private as appropriate, and keep workload label input structs private to runtime-docker implementation code.

**Test scenarios:** Existing gateway route tests and machine policy tests must keep validating the same behavior.

**Verification:** Targeted crate tests and a final public-lifetime grep audit.

### U3. Own Cross-Crate Service and Preconditions Context

**Goal:** Remove lifetime parameters from cross-crate service/precondition structs.

**Requirements:** R2, R5

**Dependencies:** None

**Files:**
- Modify: `crates/ployz-image/src/push.rs`
- Modify: `crates/ployzd/src/daemon/handlers/image/push.rs`
- Modify: `crates/ployz-orchestrator/src/deploy/execute.rs`
- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Modify: affected deploy/image tests

**Approach:** Store owned handles such as cloned IDs, paths, registries, and store drivers where cross-crate structs currently carry borrowed fields. Pass transient peer clients as method parameters where the client is not naturally owned by the service.

**Test scenarios:** Existing image push/distribute/import tests and deploy baseline precondition tests must continue to pass.

**Verification:** Targeted package tests for `ployz-image`, `ployzd`, and `ployz-orchestrator`.

### U4. Simplify Async Public Trait Lifetimes

**Goal:** Remove named public lifetimes from async trait contracts that do not return borrowed domain data.

**Requirements:** R4

**Dependencies:** None

**Files:**
- Modify: `crates/ployz-runtime-api/src/image.rs`
- Modify: `crates/ployz-runtime-api/src/mesh/mod.rs`
- Modify: runtime/backend implementations

**Approach:** Prefer lifetime-free reader aliases and elided-borrow future signatures. If an image archive reader truly must borrow backend state, keep that borrow only when compilation proves it is a real streaming contract rather than a leftover signature habit.

**Test scenarios:** Runtime API implementors must compile, and image push archive transfer tests must keep passing.

**Verification:** `cargo check` for affected packages plus final grep audit.

## Deferred to Implementation

- Whether the Docker export stream can be boxed as `'static` depends on the concrete backend stream type. Resolve by compiling the narrower API and only keep a borrowed reader if the backend requires it.
