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
ADRs 0005, 0008, and 0027; their other decisions remain in force. Route
Bindings remain gateway intent; changing external DNS is an explicit action in
the system that owns the public zone, never hidden behavior in `ployzd dns`.
