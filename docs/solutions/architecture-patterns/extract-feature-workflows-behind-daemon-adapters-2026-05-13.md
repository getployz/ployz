---
title: Extract Feature Workflows Behind Daemon Adapters
date: 2026-05-13
category: docs/solutions/architecture-patterns/
module: ployz-image
problem_type: architecture_pattern
component: service_object
severity: medium
applies_when:
  - Moving daemon-owned workflow logic into an owning domain crate
  - Correcting crate boundary checks after extracting orchestration code
  - Fixing feature-gated runtime dependencies for no-default-features test paths
related_components:
  - development_workflow
  - testing_framework
  - tooling
tags:
  - ployz-image
  - ployzd
  - crate-boundaries
  - image-workflow
  - boundary-checks
  - wireguard
  - feature-gates
---

# Extract Feature Workflows Behind Daemon Adapters

## Context

Image push and distribute behavior had grown inside `ployzd`, mixing daemon lifecycle concerns with image transfer orchestration. The refactor moved push, distribute, receive-session, and received-import workflows into `ployz-image`, while keeping active mesh lookup, runtime backend construction, image receiver listener lifecycle, and NATS node RPC transport in `ployzd`.

The same slice also exposed two boundary maintenance issues: `just test-boundaries` still referenced the deleted `ployz-runtime-backends` crate, and `ployz-wireguard-backends --no-default-features` did not fully gate userspace WireGuard implementation imports. Earlier session history on the crate-boundary plan had already identified the same architectural direction: `DaemonState` should be a router and lifecycle owner, while feature state and workflow policy belong in the subsystem that owns the feature (session history).

## Guidance

Move the workflow into the owning feature crate, but leave live daemon dependencies at the edge. The boundary shape that worked for image transfer was an owned service context plus a peer-client port passed to methods that need peer RPC:

```rust
pub struct ImageService {
    pub local_machine: MachineId,
    pub data_dir: PathBuf,
    pub operation_store: ImageOperationStore,
    pub registry: ImageRegistry,
    pub store: StoreDriver,
    pub receiver_bind_addr: Option<SocketAddr>,
}

#[async_trait]
pub trait ImagePeerClient: Send + Sync {
    async fn image_receive_session(...) -> Result<DaemonResponse, String>;
    async fn image_distribute(...) -> Result<DaemonResponse, String>;
    async fn image_received_import(...) -> Result<DaemonResponse, String>;
}
```

`ployzd` then becomes adapter code. It gathers daemon-owned resources and delegates workflow decisions to the feature crate:

```rust
fn image_service(&self, active: &ActiveMesh) -> ImageService {
    ImageService {
        local_machine: self.identity.machine_id.clone(),
        data_dir: self.data_dir.clone(),
        operation_store: self.image_operation_store(),
        registry: self.image_registry.clone(),
        store: active.mesh.store.clone(),
        receiver_bind_addr: active.image_receiver_bind_addr,
    }
}
```

Keep transport details on the daemon side of the port. `ployz-image` asks for peer image actions; `ployzd` decides those calls are NATS node RPC requests with specific subjects and timeout policies:

```rust
#[async_trait]
impl ImagePeerClient for DaemonState {
    async fn image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<DaemonResponse, String> {
        self.nats_node_rpc_client()
            .await?
            .with_policy(RpcPolicy { timeout: IMAGE_DISTRIBUTE_RPC_TIMEOUT })
            .request(
                NodeCommandSubject::image_distribute(source_machine),
                &NodeRequest::ImageDistribute { request },
            )
            .await
    }
}
```

Validation belongs at the earliest boundary that can preserve existing behavior. During this extraction, restoring the old daemon characterization tests found a behavior drift: zero-target push had started returning the inactive-mesh error before the target-required error. The adapter now rejects empty targets before active mesh lookup and before durable operation records can be created:

```rust
if request.target_machines.is_empty() {
    return self.err(
        "IMAGE_PUSH_TARGET_REQUIRED",
        "image push requires at least one target machine",
    );
}
```

## Why This Matters

This pattern keeps `DaemonState` from becoming the owner of every feature-specific state machine. Image transfer operation records, archive parsing, registry sessions, receive imports, availability writes, and multi-target distribution now live in `ployz-image`, where the code can evolve behind feature-owned tests and dependencies.

The adapter boundary also keeps trust and transport concerns visible. Public daemon API shapes stay separate from internal peer RPC, and the feature crate does not need to know about NATS subjects, RPC policies, or daemon active-mesh internals.

Boundary checks need to move with the crate graph. After deleting or replacing aggregate crates, recipes like `just test-boundaries` should reference the concrete current crates. Optional backend crates should have no-default checks that actually compile the intended empty or reduced surface.

## When to Apply

- A `ployzd` handler owns a complete feature workflow instead of routing to one.
- The feature has its own operation model, durable records, registry/store state, or cleanup rules.
- The workflow needs daemon resources but should not depend on `DaemonState`.
- Internal peer RPC should stay separate from external daemon request types.
- A crate split deletes or renames packages that CI or `just` recipes still reference.
- Optional substrate crates need proof that `--no-default-features` does not compile heavy implementation imports.

## Examples

Use characterization tests to lock behavior across the move. The restored daemon tests stayed with the adapter and caught response-order drift:

```rust
#[tokio::test]
async fn image_push_rejects_zero_targets_before_operation_side_effects() {
    let response = state.handle_image_push(&ImagePushRequest {
        target_machines: Vec::new(),
        // ...
    }).await;

    assert_eq!(response.code(), "IMAGE_PUSH_TARGET_REQUIRED");
    assert!(state.image_operation_store().list().unwrap().is_empty());
}
```

Keep the boundary recipe aligned with the crates that now own the code:

```make
test-boundaries:
    cargo check -p ployz-cert-api -p ployz-storage-api -p ployz-runtime-api -p ployz-store-api
    cargo check -p ployz-cert-acme -p ployz-storage-zfs -p ployz-host-backends
    cargo check -p ployz-wireguard-backends --no-default-features
    cargo check -p ployz-runtime-docker -p ployz-wireguard-backends
```

For no-default feature gates, gate both modules and imports. The fix in `ployz-wireguard-backends` made host/userspace items compile only when the feature is enabled, and kept the reduced crate build clean:

```rust
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
pub(crate) use ployz_error as error;

#[cfg(feature = "userspace-wg")]
pub(crate) mod host;
```

## Related

- `docs/solutions/architecture-patterns/preflight-authority-promotions-before-mutation-2026-05-08.md` — related boundary pattern for proving compatibility and ownership before mutation.
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md` — related `ployzd` adapter/routing pattern for local versus remote behavior.
- `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md` — related testing lesson about using seams and fakes to keep behavior covered without paying production costs.
