# Healthchecks Gate Only First Container Creation

A service healthcheck runs once, when a deploy first creates a service container, and only when the service actually defines one. Containers reused or adopted by later deploys are never re-gated, and deploy phase continuation never waits on healthchecks of already-running containers; a running container is healthy for deploy completion.

A newly created container without a service healthcheck must remain running through a bounded confirmation window before deploy completion. This catches immediate process exit without treating runtime survival as a service healthcheck.

A deploy can therefore complete while a routed container is not yet reachable; the gateway's unroutable-container evidence is the signal, not deploy failure. Deploy input carries no healthcheck definitions yet; when they land, they apply at creation only.
