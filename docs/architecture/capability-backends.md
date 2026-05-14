# Capability Backends

Ployz keeps feature workflow policy separate from backend mechanics.

Workflow crates choose typed capabilities. Backend crates implement those
capabilities and report unsupported operations before mutation.

## Volume Backends

| Backend | Crate | Role | Capabilities |
|---|---|---|---|
| ZFS | `crates/ployz-volume-zfs` | Product-grade Linux storage backend | ensure, remove, mount, inspect, snapshot, custom mountpoints, quota enforcement, mode enforcement, and owner enforcement. Existing ZFS transfer/clone mechanics remain ZFS-specific until the transfer protocol is lifted cleanly. |
| Docker volume | `crates/ployz-volume-docker` | Local/macOS/simple runtime backend | ensure, remove, mount, inspect through Docker named volumes. No custom mountpoint, filesystem constraint enforcement, snapshot, clone, send, or receive semantics. |
| Btrfs | `crates/ployz-volume-btrfs` | Future small-machine Linux backend | ensure, remove, mount, inspect, snapshot, clone, mode enforcement, and owner enforcement. Quota enforcement waits until the backend owns qgroup setup. No custom mountpoint or ZFS send/receive equivalent is advertised. |

`crates/ployz-volume-api` models the common contract. `crates/ployz-volume`
owns workflow preflight over those capabilities. Handlers should call workflow
services instead of matching on ZFS/Docker/Btrfs directly.

## Build Backends

| Backend | Crate | Role |
|---|---|---|
| Dockerfile | `crates/ployz-builder-dockerfile` | Plans Dockerfile `docker build` invocations. |
| Railpack | `crates/ployz-builder-railpack` | Plans Railpack prepare plus Docker BuildKit invocations. |

`crates/ployz-build-api` owns shared command-plan types. `crates/ployz-build`
owns input validation, operation records, artifact rendering, redaction policy,
and image availability recording.

## Certificate Issuers

| Issuer | Crate | Role |
|---|---|---|
| ACME | `crates/ployz-cert-acme` | ACME account, order, HTTP-01 challenge, and finalize behavior. |
| Static/imported | `crates/ployz-cert-static` | Local/dev and externally managed certificate material. |

`crates/ployz-cert-api` owns the generic `CertificateIssuer` contract while
retaining ACME-specific traits for the existing two-step ACME workflow. New
issuers should not add ACME account/order/challenge concepts to the generic
issuer surface.

## Boundary Rules

- Capability API crates do not import `ployzd` or `ployz-api`.
- Workflow crates may depend on capability APIs and model crates, not daemon
  handler modules.
- Backend crates implement capability APIs and keep implementation-specific
  identifiers local.
- Unsupported backend operations return typed unsupported-capability errors
  before mutation.
