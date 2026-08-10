# Route Bindings Are Service References, Not Deploy Children

> Current v2 amendment: standalone route attach/remove now write binding rows directly. Deploy only creates a missing deterministic automatic binding and does not reconcile a manifest route set. Gateways join the binding's service directly to containers from `service.active_deploy`; no serving-target or revision-entry layer remains.

Ployz Route Bindings attach external hostnames to named services, not to deploy manifests or currently-running containers. Gateways resolve a binding's service through the current Service row and serve only containers from its active deploy (ADR 0023).

Standalone route attach/remove commands own binding lifecycle. A deploy may insert a missing deterministic automatic binding for a requested service, but deploy input has no route set and deploy never updates or removes an existing binding. Removing a service therefore does not erase independently named routing intent.

If a binding points at a service that is absent from the current serving target, gateways keep the binding and return an unavailable response for it instead of treating the route as invalid state.
