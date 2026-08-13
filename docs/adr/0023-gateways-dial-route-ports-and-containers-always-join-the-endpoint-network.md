# Gateways Dial Route Ports And Containers Always Join The Endpoint Network

> Current v2 amendment: route attach/remove are standalone preferred-controller mutations. A full namespace deploy inserts missing deterministic automatic port-80 bindings and otherwise leaves Route Bindings alone. Gateways select a named namespace/service from one Namespace intent document and require machine endpoint testimony to name that service's active deploy; there is no namespace-revision lookup or Container row.

Every service container joins the gateway endpoint network unconditionally at creation; ports never influence network membership. Gateway upstream matching selects machine-published endpoints by the Route Binding's namespace/service names and requires each endpoint's deploy name to equal the Namespace service entry's `active_deploy`, then dials the observed IP on the Route Binding's endpoint port. The port declared or exposed by the container itself is not part of upstream matching and carries no access-control meaning.

A routed endpoint port change is therefore a standalone Route Binding mutation with no container replacement, per-container plan step, or machine-side action. Accepted trade-offs: a reroute applies directly with no health gate, so a port nothing listens on fails at the traffic layer without mutation-time evidence, and every container consumes an endpoint-network address whether or not it is ever routed.

Container IP and deploy identity are facts about the container; the active deploy is a fact about the service; endpoint port is a fact about the route. Deploy's automatic-hostname behavior and the standalone route-attach primitive write the same Route Binding row shape.
