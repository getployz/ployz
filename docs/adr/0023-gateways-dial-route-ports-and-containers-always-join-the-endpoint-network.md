# Gateways Dial Route Ports And Containers Always Join The Endpoint Network

Every service container joins the gateway endpoint network unconditionally at creation; ports never influence network membership. Gateway upstream matching selects containers by the Route Binding's service row id and requires each container's deploy id to equal that service row's `active_deploy`, then dials the container's observed IP on the Route Binding's endpoint port. The port declared or exposed by the container itself is not part of upstream matching and carries no access-control meaning.

A routed endpoint port change is therefore an endpoint reroute: a route-level KV state commit inside an ordinary manifest deploy, with no container replacement, no per-container plan step, and no machine-side action; route attach/detach to already-running containers is likewise pure route state. Accepted trade-offs: a reroute applies directly with no health gate, so a port nothing listens on fails at the traffic layer without deploy-time evidence, and every container consumes an endpoint-network address whether or not it is ever routed.

Container IP and deploy identity are facts about the container; the active deploy is a fact about the service; endpoint port is a fact about the route. Deploy's automatic-hostname behavior and the standalone route-attach primitive write the same Route Binding row shape.
