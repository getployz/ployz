---
title: Lifetime-Free Public Rust API Boundaries
date: 2026-05-13
category: docs/solutions/architecture-patterns/
module: ployz-runtime-api, ployz-image, ployz-gateway, ployz-orchestrator
problem_type: architecture_pattern
component: service_object
severity: medium
applies_when:
  - Exporting Rust contracts across crate boundaries
  - Moving implementation details behind crate-private or test-private helpers
  - Replacing borrowed public structs or traits with owned API shapes
  - Fixing public trait signatures that leak named lifetimes to downstream crates
related_components:
  - development_workflow
  - testing_framework
  - tooling
tags:
  - rust
  - public-api
  - lifetimes
  - crate-boundaries
  - owned-types
  - imagearchivereader
  - dockerimageref
  - service-context
---

# Lifetime-Free Public Rust API Boundaries

## Context

The public API audit on `feat/idiomatic-crate-boundaries` found several exported Rust contracts that exposed lifetime parameters for data that was really boundary state: parsed Docker image references, gateway TLS listener configuration, deploy preconditions, image archive readers, image service context, and async trait methods.

That shape made implementation borrowing part of the caller contract. The cleanup followed the same direction as `docs/plans/2026-05-13-005-refactor-owned-public-apis-plan.md` and the adjacent daemon-adapter pattern: public APIs should own the data they return or store, while borrows stay inside implementation logic.

## Guidance

Public API structs and aliases should own boundary data. A parser can accept `&str`, but its public result should be an owned value. Borrow from that owned value only at the implementation call site that needs a borrow:

```rust
pub fn parse(input: &str) -> Result<Foo, Error> {
    let borrowed = parse_impl(input)?;
    Ok(Foo::from_borrowed(borrowed))
}
```

For public async stream or reader aliases, use a lifetime-free owned alias instead of making downstream callers thread lifetimes through their own APIs:

```rust
pub type ImageArchiveReader = Pin<Box<dyn AsyncRead + Send + 'static>>;
```

Public configuration structs should also own their fields. The gateway keeps `run_server` public, but its TLS listener argument is now an owned public type. It borrows optional path fields only while configuring TLS:

```rust
pub struct GatewayTlsListener {
    pub listen_addr: String,
    pub static_cert_path: Option<String>,
    pub static_key_path: Option<String>,
}

pub fn run_server(
    opt: Opt,
    listen_addr: &str,
    tls_listener: Option<GatewayTlsListener>,
    threads: usize,
    metrics_listen_addr: Option<&str>,
    shared_snapshot: SharedSnapshot,
) -> Result<(), GatewayError> {
    if let Some(tls_listener) = tls_listener {
        let paths = (
            tls_listener.static_cert_path.as_deref(),
            tls_listener.static_key_path.as_deref(),
        );
        // Borrow only inside the implementation.
    }

    Ok(())
}
```

Precondition envelopes should own durable domain state and borrow only when validating:

```rust
#[derive(Debug, Clone, Default)]
pub struct DeployApplyPreconditions {
    pub expected_baseline: Option<DeployPreviewBaseline>,
}

ensure_deploy_baseline(preconditions.expected_baseline.as_ref(), &initial_plan)?;
```

When a borrowed helper is only for local implementation or tests, lower its visibility instead of making it part of the public API:

```rust
#[cfg(test)]
fn match_http_route<'a>(
    snapshot: &'a GatewaySnapshot,
    host: Option<&str>,
    path: &str,
) -> Option<&'a HttpRouteView> {
    // Test-only borrowed view into a snapshot.
}
```

Stateful feature services follow the same rule: own stable context, then accept short-lived collaborators as method parameters. See `docs/solutions/architecture-patterns/extract-feature-workflows-behind-daemon-adapters-2026-05-13.md` for the daemon adapter version of this pattern.

## Why This Matters

Lifetimes in public signatures are API commitments. They force every caller to understand and preserve the implementation's borrowing relationship, and changing that relationship later becomes a breaking API change.

Owned public types keep crate boundaries honest: callers pass or receive domain values, while the implementation chooses when to borrow. That makes async tasks, daemon handlers, runtime backends, peer RPC adapters, and SDK/API consumers easier to compose because ownership pressure stays local.

This also reinforces the repository architecture rule that public protocol and service state should be explicit, typed, and stable. Borrowed views are fine behind crate-private seams, but public contracts should not expose incidental stack-frame relationships.

## When to Apply

- A `pub struct`, `pub enum`, `pub type`, or public trait method exposes `<'a>` only to point at caller-owned strings, paths, slices, readers, or context.
- A value crosses crates, daemon handlers, runtime backend traits, peer RPC, async task boundaries, or SDK/API surfaces.
- A parser accepts borrowed input but returns a reusable parsed value.
- A service struct stores context that previously borrowed config, store handles, registries, machine IDs, paths, or preconditions.
- A command, request, listener, reader, or runtime handle may outlive the stack frame that created it.

Keep borrows when:

- The function is private or `pub(crate)` and purely local.
- The borrow is the operation input, such as `inspect_image(&self, reference: &str)`.
- The helper is `#[cfg(test)]` or test-only setup.
- The API intentionally uses static data, such as `&'static str` error codes or metric names.

## Examples

Before: parsed Docker image references exposed borrowed fields from the input string.

```rust
pub struct DockerImageRef<'a> {
    pub from_image: &'a str,
    pub tag: Option<&'a str>,
}
```

After: parsing borrows input, while the result owns boundary data.

```rust
pub struct DockerImageRef {
    pub from_image: String,
    pub tag: Option<String>,
}
```

Before: archive readers made every runtime backend and caller carry the reader lifetime.

```rust
pub type ImageArchiveReader<'a> = Pin<Box<dyn AsyncRead + Send + 'a>>;
```

After: archive readers are owned runtime objects.

```rust
pub type ImageArchiveReader = Pin<Box<dyn AsyncRead + Send + 'static>>;
```

Before: docs and service shapes advertised `ImageService<'a>` as the feature boundary.

```rust
pub struct ImageService<'a> {
    pub local_machine: &'a MachineId,
    pub data_dir: &'a Path,
    pub registry: &'a ImageRegistry,
    pub peer_client: &'a dyn ImagePeerClient,
}
```

After: the service owns stable context, and peer RPC is a borrowed operation collaborator.

```rust
pub struct ImageService {
    pub local_machine: MachineId,
    pub data_dir: PathBuf,
    pub registry: ImageRegistry,
    pub store: StoreDriver,
}
```

The service extraction details are covered in `docs/solutions/architecture-patterns/extract-feature-workflows-behind-daemon-adapters-2026-05-13.md`; this doc's boundary rule is that the public service type itself stays lifetime-free.

## Verification

The cleanup was verified with:

- `cargo check -p ployz-runtime-api -p ployz-runtime-docker -p ployz-wireguard-backends -p ployz-image -p ployz-orchestrator -p ployz-gateway -p ployzd`
- `cargo test -p ployz-image`
- `cargo test -p ployzd image_push --lib`
- `cargo test -p ployz-runtime-docker image_ref`
- `cargo test -p ployz-gateway routes::tests::match`
- `cargo test -p ployz-orchestrator http01_challenge_visibility`
- `cargo test -p ployz-cert-acme http01_challenge_visibility`
- Grep audit for true public lifetime APIs and stale docs references:

```bash
rg -n "pub (struct|enum|type|trait) [A-Za-z0-9_]+<'|pub fn [A-Za-z0-9_]+<'|pub async fn [A-Za-z0-9_]+<'|ImageArchiveReader<'|ImageService<'|DockerImageRef<'|DeployApplyPreconditions<'|GatewayTlsListener<'" crates docs
```

## Related

- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md` — public API shape is a durable promise, and exported variants or fields should represent real states the system can produce.
- `docs/solutions/architecture-patterns/extract-feature-workflows-behind-daemon-adapters-2026-05-13.md` — owned feature service context and daemon adapter boundaries.
- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md` — typed precondition payloads and checked transitions before mutation.
