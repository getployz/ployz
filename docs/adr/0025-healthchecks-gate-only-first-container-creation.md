# Healthchecks Gate Only First Container Creation

A service healthcheck runs once, when a deploy first creates a service container, and only when the service actually defines one. Containers reused or adopted by later deploys are never re-gated, and deploy phase continuation never waits on healthchecks of already-running containers; a running container is healthy for deploy completion. This supersedes the part of ADR 0008 that required new containers to pass healthchecks "before the phase can continue" as a recurring gate.

The trade-off is accepted deliberately: a deploy can complete while a routed container is not yet reachable, and the gateway's unroutable-container evidence is the signal, not deploy failure. In exchange, deploys that reuse containers stay fast and cannot be wedged by a healthcheck that a previously-promoted container would no longer pass. Deploy input carries no healthcheck definitions yet; when they land, they apply at creation only.
