# Keeper Owns The Machine Network And Admission Reads Neutral Testimony

Keeper owns the network on one machine: the WireGuard interface and its peers,
the eBPF attachment and its route map, the host sysctls and forwarding rules,
and the endpoint bridge. Worker owns the containers: Docker, builds, volumes,
endpoints, and logs. The split follows privilege, not subject matter. Anything
requiring the host network namespace or a host capability belongs to Keeper;
everything else belongs to Worker, which needs a Docker socket and its state
paths and nothing more.

The peer set and the eBPF route map change together, for the same reason, when
cluster membership changes. They are one concern and one owner. Keeper applying
the declared Dataplane Projection is the same pattern as Keeper converging a
machine assignment — Control decides and Keeper enforces — so this adds no new
mechanism and does not widen Keeper's authority beyond enforcing recorded
decisions.

Placement admission and dataplane testimony are provider-neutral. Admission
asks whether a machine's dataplane is ready, which peers it has, and how fresh
they are. It does not ask for kernel WireGuard peer keys or netlink handshake
ages, and no answer names an implementation:

```text
dataplane testimony
  readiness        ready | degraded(reason) | absent
  peers            [{ peer identity, last-verified age }]
  reachable subnets
```

`absent` is a legal answer. A machine with no mesh is a reduced-capability
member, the way a machine without a storage pool is a reduced-capability
member. It is not admissible for placement that requires peer reachability, and
it is not rejected from the cluster. This is what makes a single-machine
cluster, a development machine, and a host whose kernel cannot carry the native
mesh expressible as members rather than as failures.

The reason to fix the contract before the topology is that they have different
costs. Where a process runs is a refactor inside one shipped binary. The
admission types are core policy exported to the SDK, so a WireGuard-shaped
answer is a public contract that cannot be narrowed later without breaking
consumers. `PloyzNativeMeshReady` describes one implementation and must not be
the shape of the answer.

A neutral contract designed against one implementation acquires that
implementation's shape. The test is to express the second implementation's
answer before accepting the first: a tailnet reports peers by node key rather
than by WireGuard public key, reports liveness on its own schedule, and carries
advertised-route and access-policy state the native mesh has no equivalent for.
If the contract cannot carry both without branching on the implementation, it is
not yet neutral.

This refines ADR 0035 rather than replacing it. Fresh testimony still gates new
placement, gathered at the point of use from the declared machine set; only the
shape of the testimony changes. The native mesh remains the only implementation,
and adopting another remains a cluster-wide Dataplane Provider Transition rather
than a per-machine choice.
