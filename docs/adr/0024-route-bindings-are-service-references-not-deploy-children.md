# Route Bindings Are Service References, Not Deploy Children

Ployz route bindings attach external routes to service ids, not to deploy manifests, namespace revision entries, or currently-running service containers. Route binding state therefore carries no namespace revision entry id: gateways resolve a binding's service id through the current serving target to find the entry identity whose containers may serve it (ADR 0023). Pinning an entry id inside the binding would silently detach routes whenever a service's entry identity moved on without a matching route rewrite.

Deploy manifests are currently the only writer of route binding state, and they write it declaratively: a deploy commits every binding its manifest declares and removes bindings for targets no service in the manifest declares. Future standalone route operations must update the same per-target binding state; they do not get a separate store or a merge rule.

If a binding points at a service that is absent from the current serving target, gateways keep the binding and return an unavailable response for it instead of treating the route as invalid state.
