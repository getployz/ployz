# Compose Support

Ployz treats Docker Compose as a deploy input adapter, not as the core orchestration model. Compose terms may become core Ployz language when they match Ployz semantics, but Compose project structure, unsupported lifecycle behavior, and adapter extensions stay at the boundary.

This page is the living support contract for Compose input. It should classify Compose features as supported, limited, unsupported, or Ployz-specific extension as the adapter is implemented.

## Initial Shape

Supported core vocabulary:

- services
- replicated and global service modes
- replicas
- volumes
- configs
- secrets
- healthchecks
- ports
- update order
- pre-start hooks through a Compose-facing `x-pre_deploy` extension

Known boundary rules:

- A Compose project maps to one Ployz namespace; Project is not core Ployz language.
- `depends_on` may help derive deploy phases, but Compose conditions such as `service_healthy` and `service_completed_successfully` do not become durable workflow state.
- `x-pre_deploy` maps to a Ployz pre-start hook: a service-scoped one-off command that runs before new service containers are started.
- A pre-start hook runs on the same machine as the first planned run or replace action for that service.
- A successful pre-start hook is not remembered across deploy attempts. If a later retry still plans run or replace work for that service, the hook runs again, so hook commands should be safe to retry.
- Failed pre-start hook containers are retained as failed deploy evidence and are not service containers for replica satisfaction, routing, or DNS.
- `stop_grace_period` maps to Ployz stop grace period. When omitted, Ployz defaults to 10 seconds.
- Fresh role observations initially use a 30 second platform freshness TTL for warning-only role observation windows.
- Compose networks are not the primary Ployz deploy boundary.
- Route bindings remain Ployz concepts; ports do not imply attached routes.
- Ployz extensions should stay adapter-level unless the concept becomes core language.

## Deploy Results

Core deploy evidence should include first-class service deploy results:

- `completed`: service had planned work and it succeeded.
- `failed`: service had planned work and failed.
- `skipped`: service had planned work but was not reached because an earlier phase failed.
- `unchanged`: service was observed and already matched desired state, so no work was needed.
- `removed`: service was present in runtime state but not in the namespace revision, and deploy removed its runtime containers.

Warning evidence belongs to the namespace deploy outcome and operation events, not to individual service deploy results.
Role observation window non-convergence is warning evidence and can make the namespace deploy outcome `completed_with_warnings` or `partially_completed_with_warnings`.

Core deploy evidence should include first-class namespace deploy outcomes:

- `completed`: all planned phases completed without warning evidence.
- `completed_with_warnings`: all planned phases completed with warning evidence.
- `partially_completed`: at least one phase promoted, then a later phase failed.
- `partially_completed_with_warnings`: at least one phase promoted, then a later phase failed, with warning evidence.
- `failed`: no phase promoted before failure.
- `cancelled`: deploy was cancelled before a normal terminal outcome.

For automation, `completed` and `completed_with_warnings` are successful terminal deploy outcomes. `partially_completed` and `partially_completed_with_warnings` are non-success outcomes with useful namespace progress. `failed` is failure, and `cancelled` is cancellation.

Useful namespace progress means at least one phase promoted. Started containers, completed hooks, or created volumes do not by themselves make a deploy partially completed.

Removals for services or containers outside the namespace revision run as final cleanup after desired service phases promote. Removed services remain in the serving target until cleanup performs serving unpublish. For routed or DNS-published services, cleanup removes the service from serveable surfaces first, observes gateway and DNS role processes through the role observation window as warning evidence, then stops and removes runtime containers. A service's deploy result becomes `removed` only after its runtime containers are removed. If cleanup fails, the namespace deploy is failed or partially completed using the same phase-promotion rule.
