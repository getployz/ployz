# Gateways Dial Route Ports And Containers Always Join The Endpoint Network

Every service container joins the gateway endpoint network unconditionally at creation; ports never influence network membership. Gateway upstream matching selects containers by service id and namespace revision entry identity, then dials the container's observed IP on the active route's endpoint port — the port declared or exposed by the container itself is not part of upstream matching and carries no access-control meaning.

A routed endpoint port change is therefore an endpoint reroute: a route-level KV state commit inside an ordinary manifest deploy, with no container replacement, no per-container plan step, and no machine-side action; route attach/detach to already-running containers is likewise pure route state. Accepted trade-offs: a reroute applies directly with no health gate, so a port nothing listens on fails at the traffic layer without deploy-time evidence, and every container consumes an endpoint-network address whether or not it is ever routed.

Container IP is a fact about the container; endpoint port is a fact about the route. Deploy input remains the single declarative writer of route state (`routes` per service, one active route record per target); standalone route operations are deferred and must write the same route state when they arrive.
