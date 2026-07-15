# Public Ingress DNS Is External

Ployz DNS resolves ordinary internal service names from serving intent and
machine facts and forwards other queries to the machine's upstream resolver. It
does not publish Route Binding hostnames or gather gateway endpoints to construct
public DNS answers. Operators or a managed Ployz DNS product configure public
ingress DNS outside the cluster DNS role and point those names at gateway
machines.

The DNS role therefore keeps no route-hostname projection, last-known-good public
answer table, route projection health, public listener, or compatibility shim.
It continues to use the intent drumbeat and mirror for internal service-name
resolution, exposes machine-scoped resolve and status RPC, and preserves upstream
forwarding and NATS failover behavior.

For DNS, this supersedes the public route-answer and DNS-projection clauses in
ADRs 0001, 0005, 0008, 0009, 0018, 0027, 0028, 0030, and 0031. Their gateway,
internal DNS, recovery, invalidation, and operation-evidence decisions remain in
force.

This removes a second, incomplete owner for public ingress DNS. Route Bindings
remain gateway intent, and changing external DNS remains an explicit action in
the system that owns the public zone rather than hidden behavior in `ployzd dns`.
