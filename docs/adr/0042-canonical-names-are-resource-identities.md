# Canonical Names Are Resource Identities

Ployz uses canonical operator-visible names, not generated identifiers, as the identities of cluster resources. The Preferred Controller serializes ordinary writes; if a partition creates competing controllers, Corrosion converges competing whole-document writes to the same semantic key and the losing intent may disappear without a retained shadow. Randomness remains only for secrets, cryptographic material, Corrosion internals, and external runtime handles.

## Consequences

- Cluster, machine, peer, token, namespace, route, deploy, and managed-container identities are canonical names or composites of canonical names. There are no Ployz ULID identities or `RowId` types.
- A name is immutable identity, not a mutable display handle. Deleting and recreating the same name continues that logical identity; a genuinely fresh resource needs a fresh name.
- A service is identified by `<namespace>/<service>`. Services are nested in one complete Namespace intent document so a full multi-service deploy has one atomic publication; storage nesting does not merge their identities.
- A deploy is a complete namespace desired-state snapshot containing every service that should remain. Deploy names are caller-visible, namespace-scoped, and one-shot; retrying after an accepted attempt uses a fresh name so changed intent cannot masquerade as the same operation.
- A managed replica is identified by `<namespace>/<service>/<deploy>/<slot>` on one named machine. Docker's random container id stays private to that machine's runtime adapter and never crosses a Ployz RPC or row boundary.
- Controller appointments use the preferred machine name plus a weak heartbeat timestamp. The timestamp guides advisory replacement; it is not a lease, term, or fence.
- In-place tombstone reaping remains forbidden. Refound into a fresh cluster is the compaction mechanism.
- Join admission is a cluster mutation routed through the Preferred Controller. Cross-row mesh-key or subnet conflicts caused by split controllers fail closed and require explicit repair; Ployz does not run a post-write election or automatic reallocation protocol.
