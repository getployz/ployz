# Gateway Projection Is Per-Route And Failure-Isolated

Ployz gateways should apply serving inputs as independent route projections where possible, not as one global all-or-nothing generated config. If one route projection cannot be applied, the gateway should report failed gateway observation for that route while continuing to apply and serve unrelated valid routes.

This keeps the gateway dumb without making a single bad route, missing local artifact, or runtime apply error poison the whole gateway. Route and certificate operations should prevent invalid route bindings before promotion, so per-route failure isolation is failure containment rather than the normal correctness path.
