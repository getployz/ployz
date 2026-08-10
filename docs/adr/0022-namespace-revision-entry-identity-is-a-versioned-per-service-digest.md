# Namespace Revision Entry Identity Is A Versioned Per-Service Digest

> Superseded for current v2 by [ADR 0043](0043-runtime-reality-comes-from-node-rpc.md), whose one Namespace intent document selects each service's active deploy and whose node RPCs use natural managed-replica identities; the revision-entry digest model below is historical.

Container replacement is decided by an opaque, versioned SHA-256 identity,
not by ad hoc field-by-field comparisons. The identity is scoped by namespace
and service, so otherwise-identical services cannot share a replacement key
across either boundary. It can therefore travel as a standalone value through
container labels, projections, and observations whose readers do not have the
full deploy input.

The currently implemented canonical encodings are namespace revision entry v11
(`ployz.namespace_revision_entry.v11`) and namespace revision v9
(`ployz.namespace_revision.v9`). Every value is length-framed with its field
tag. Entry v11 covers namespace id, service id, the final image reference, and
the create-time runtime shape: command, entrypoint, stop grace period, volume
mounts, healthcheck, restart policy, canonicalized capability additions and
drops, resource limits, and the environment contribution. The removed
pushed-image form also covered the platform-independent image index identity. It
deliberately
excludes service mode and replica count, pre-start hooks, dependencies, route
targets, and routed endpoint ports; those can change reconciliation without
changing an individual container's create-time shape. In particular, endpoint
ports remain route state under [ADR 0023](0023-gateways-dial-route-ports-and-containers-always-join-the-endpoint-network.md).

Namespace v9 covers the complete normalized namespace reconciliation input. It
starts with namespace id, then visits services in service-id order. For each
service it covers service id, final image reference, replicated/global mode and
replica count, the same create-time runtime shape and environment contribution,
the optional pre-start command, sorted and deduplicated dependencies with their
conditions, and routes sorted by target and endpoint port. Placement hints that
do not define the desired namespace, such as `keep`, are excluded.

The version marker is part of each digest. Adding, removing, reordering, or
renormalizing any covered frame requires an explicit encoding-version bump and
new golden expectations. An existing version must never be reinterpreted. This
makes a compatibility-breaking replacement decision visible in code review and
prevents an apparently innocuous upgrade from silently changing the identity of
every running container.

[ADR 0040](0040-corrosion-replaces-the-core-and-nats.md) kept this identity law
but replaced two v1 mechanisms. The later v2 Namespace service entry carries environment
names mapped to lowercase SHA-256 fingerprints; changing a name or fingerprint
changes the replacement-relevant content. This intentionally prices dictionary
exposure for low-entropy values into the single-operator membership trust
ceiling. It does not revive the Controller-seed-derived HMAC, its process-local
key, or its restart and promotion behavior. Empty environment maps contribute no
environment fingerprints.

Likewise, v2 Namespace service entries carry the digest-pinned image reference itself. The
old pushed-image receipt index, including its per-platform manifest and image-id
frames, belonged to the removed Control build-receipt path and is not a v2
identity component. Mutable image tags must be resolved before replacement
identity is committed. The v11 and v9 constants name the incumbent encodings; a
Corrosion-era derivation that substitutes these v2 source fields must use a new
version marker rather than reinterpret v11 or v9. This preserves the accepted
encoding boundary without preserving Controller or receipt machinery that ADR
0040 removed.
