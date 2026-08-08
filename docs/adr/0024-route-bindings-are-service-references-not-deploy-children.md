# Route Bindings Are Service References, Not Deploy Children

> Current v2 amendment: standalone route attach/remove now write binding rows directly. Deploy only creates a missing deterministic automatic binding and does not reconcile a manifest route set. Gateways join the binding's service directly to containers from `service.active_deploy`; no serving-target or revision-entry layer remains.

Ployz route bindings attach external routes to service ids, not to deploy manifests, namespace revision entries, or currently-running service containers. Route binding state therefore carries no namespace revision entry id: gateways resolve a binding's service id through the current serving target to find the entry identity whose containers may serve it (ADR 0023).

Deploy manifests are currently the only writer of route binding state, and they write it declaratively: a deploy commits every binding its manifest declares and removes bindings for targets no service in the manifest declares. Future standalone route operations must update the same per-target binding state; they do not get a separate store or a merge rule.

If a binding points at a service that is absent from the current serving target, gateways keep the binding and return an unavailable response for it instead of treating the route as invalid state.
