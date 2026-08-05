# Retired NATS Control Plane

The NATS-backed v1 control plane is not part of the current workspace or
product architecture. [ADR 0040](../adr/0040-corrosion-replaces-the-core-and-nats.md)
supersedes it with Corrosion rows over HTTP/JSON/SSE on the WireGuard mesh.

Current path ownership and dependency direction live in
[`code-map.md`](code-map.md). The surviving rationale is consolidated in ADR
0040; deleted v1 records remain available in git history and are not
implementation guidance for v2.
