# Compose Support

Ployz treats Docker Compose as a deploy input adapter, not as the core orchestration model. Compose terms may become core Ployz language when they match Ployz semantics, but Compose project structure, unsupported lifecycle behavior, and adapter extensions stay at the boundary.

This page is the living support contract for Compose input. It should classify Compose features as supported, limited, unsupported, or Ployz-specific extension as the adapter is implemented.

## Current Adapter Contract

Supported and translated fields become a Ployz deploy request. Unsupported and unknown fields are diagnostics; strict mode rejects them before submit unless `--allow-unsupported` is set. Invalid values are always fatal.

| Compose field | Status | Guidance |
| --- | --- | --- |
| `name` | Translated | Maps to the Ployz namespace unless `deploy -n` overrides it. |
| `services` | Translated | Each service maps to one Ployz deploy service. |
| `version` | Supported | Obsolete Compose metadata; ignored with an advisory. |
| `services.*.image` | Translated | Required; maps to the service image reference. |
| `services.*.command` | Translated | Shell form is split into argv; exec form must contain scalar values. |
| `services.*.entrypoint` | Translated | Shell form is split into argv; empty string or empty exec form clears the image entrypoint. |
| `services.*.environment` | Translated | Map and list forms are merged into container environment. |
| `services.*.env_file` | Translated | Files are read relative to the Compose file and merged into container environment. |
| `services.*.deploy.replicas` | Translated | Maps to Ployz replica count; omitted defaults to 1. |
| `services.*.deploy.resources.limits` | Translated | `cpus`, `memory`, and `pids` map to Docker container limits. |
| `services.*.deploy.restart_policy.condition` | Translated | Maps to Docker restart policy when `services.*.restart` is absent. |
| `services.*.stop_grace_period` | Translated | Maps to Ployz stop grace period; omitted defaults to 10 seconds. |
| `services.*.cap_add` | Translated | Adds Linux capabilities to the created container. |
| `services.*.cap_drop` | Translated | Drops Linux capabilities from the created container. |
| `services.*.healthcheck` | Translated | Maps to Docker healthcheck and gates only newly-created containers. |
| `services.*.restart` | Translated | Maps to Docker restart policy. |
| `services.*.depends_on` | Translated | Short form and `service_started` map to `started`; `service_healthy` maps to `healthy` and requires the target service to define an executable healthcheck. `service_completed_successfully` is rejected. |
| `services.*.pre_start` | Translated | Runs one retry-safe hook before new containers for that service. Failed hook containers are retained as operation evidence. |
| `services.*.x-ports` | Translated | `PORT[/https]` uses the service name as the automatic hostname label; `auto:PREFIX:PORT[/https]` selects a label; `HOST:PORT[/https]` declares a hostname. |
| `services.*.build` | Unsupported (planned) | build images before deploy |
| `services.*.cgroup_parent` | Unsupported (unsupported) | cgroup parent is not part of the deploy model |
| `configs`, `services.*.configs` | Unsupported (planned) | configs are not deployed yet |
| `services.*.deploy.mode` | Unsupported (planned) | global deploy mode is not deployed yet |
| `services.*.deploy.placement` | Unsupported (planned) | placement constraints are not deployed yet |
| `services.*.deploy.resources.reservations` | Unsupported (planned) | reservations are not deployed yet |
| `services.*.deploy.restart_policy` subfields other than `condition` | Unsupported (planned) | restart policy delay/window fields are not deployed yet |
| `services.*.deploy.update_config` | Unsupported (planned) | update order is not deployed yet |
| `services.*.devices` | Unsupported (unsupported) | host capability controls are not deployed yet |
| `services.*.dns` | Unsupported (unsupported) | custom DNS settings are not deployed yet |
| `services.*.dns_search` | Unsupported (unsupported) | custom DNS settings are not deployed yet |
| `services.*.expose` | Unsupported (unsupported) | use x-ports for ingress in this slice |
| `services.*.extra_hosts` | Unsupported (unsupported) | extra hosts are not deployed yet |
| `services.*.init` | Unsupported (unsupported) | init process selection is not deployed yet |
| `services.*.labels` | Unsupported (unsupported) | container labels are owned by Ployz |
| `services.*.logging` | Unsupported (unsupported) | logging driver settings are not deployed yet |
| `services.*.networks` | Unsupported (planned) | custom networks are not deployed yet |
| `services.*.platform` | Unsupported (unsupported) | platform selection is not deployed yet |
| `services.*.ports` | Unsupported (planned) | use x-ports for ingress in this slice |
| `services.*.privileged` | Unsupported (unsupported) | host capability controls are not deployed yet |
| `services.*.profiles` | Unsupported (planned) | profile resolution is deferred |
| `services.*.pull_policy` | Unsupported (unsupported) | pull policy is not deployed yet |
| `secrets`, `services.*.secrets` | Unsupported (planned) | secrets are planned separately |
| `services.*.security_opt` | Unsupported (unsupported) | host capability controls are not deployed yet |
| `networks` | Unsupported (planned) | top-level networks are not deployed yet |
| `volumes` | Unsupported (planned) | top-level named volumes are not deployed yet |
| `services.*.ulimits` | Unsupported (unsupported) | ulimits are not deployed yet |
| `services.*.user` | Unsupported (unsupported) | container user is not deployed yet |
| `services.*.volumes` | Unsupported (planned) | volumes are not deployed yet |
| `services.*.working_dir` | Unsupported (unsupported) | working directory is not deployed yet |
| `services.*.x-pre_deploy` | Unsupported | rename the hook to Compose `pre_start` |
| Any other field | Unsupported | Unknown field; remove it or pass `--allow-unsupported`. |

## Environment

`.env` beside the Compose file is loaded for interpolation and variable resolution. OS environment values override `.env` values. `.env` does not automatically become container environment.

Container environment is built per service in this order:

1. `env_file` entries in listed order. Later files win over earlier files.
2. `environment` entries. Inline environment wins over `env_file`.

`env_file` paths are relative to the Compose file. A missing env file is invalid unless the long form sets `required: false`. Long-form `env_file` options other than `path` and `required` are unknown fields.

Environment map values must be scalar or null. A null map value such as `FOO:` resolves from the CLI process environment after `.env` overlay; when it is unset, Ployz omits it and emits an advisory. List entries may be `KEY=VALUE` or bare `KEY`; bare entries resolve the same way as map null values.

Environment is plaintext deploy input and is stored in operation evidence. Use it for non-sensitive configuration; secrets are the planned mechanism for sensitive values.

## Interpolation

Ployz applies YAML merge keys, then interpolates string scalars, then parses typed Compose fields.

Supported interpolation forms:

| Form | Meaning |
| --- | --- |
| `$VAR` | Use `VAR`, or empty string with an advisory when unset. |
| `${VAR}` | Use `VAR`, or empty string with an advisory when unset. |
| `${VAR:-default}` | Use `default` when `VAR` is unset or empty. |
| `${VAR-default}` | Use `default` when `VAR` is unset. |
| `${VAR:?message}` | Invalid when `VAR` is unset or empty. |
| `${VAR?message}` | Invalid when `VAR` is unset. |
| `$$` | Literal `$`. |

Variable names start with a letter or `_` and continue with letters, digits, or `_`. Braced expressions end at the first `}`. Nested `${...}` inside a braced expression is invalid, and defaults cannot contain `}`.

## Diagnostics

Strict mode is the default. It rejects any unsupported or unknown field and renders all findings sorted by path. Advisory findings are never fatal.

`--allow-unsupported` downgrades `Unsupported` and `UnknownField` findings to warnings and submits the supported subset. `InvalidValue` findings always reject the file, including in allow mode.

Rejections render one finding per line:

```text
services.web.healthcheck  unsupported (planned)  healthchecks are parsed but not deployed yet; remove it or pass --allow-unsupported
services.web.mystery  unknown field  remove it or pass --allow-unsupported
```

Warnings in allow mode carry a `warning` token after the path:

```text
services.web.healthcheck  warning  unsupported (planned)  healthchecks are parsed but not deployed yet; remove it or pass --allow-unsupported
```

## Boundary Rules

Known boundary rules:

- A Compose project maps to one Ployz namespace; Project is not core Ployz language.
- Compose networks are not the primary Ployz deploy boundary.
- Route bindings remain Ployz concepts; ports do not imply attached routes.
- Ployz extensions should stay adapter-level unless the concept becomes core language.

## Deploy Results

Dependencies divide a namespace deploy into topological phases. Every service
whose dependencies are satisfied at the same point belongs to the same phase.
After every service in a phase passes its creation gate, Ployz atomically
promotes that phase's Serving Target entries and route-binding changes. Later
phases reach promoted dependencies through ordinary internal service DNS.
`started` remains subject to the dependency service's own creation gate;
`healthy` requires an executable healthcheck. Completion-gated jobs are not
part of the deploy model.

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

Removals for services or containers outside the namespace revision run as final cleanup after desired service phases promote. Removed services remain in the serving target until cleanup performs serving unpublish. For routed services, cleanup removes the service from gateway eligibility first, observes gateway role processes through the role observation window as warning evidence, then stops and removes runtime containers. Internal DNS follows the serving target and does not publish route hostnames. A service's deploy result becomes `removed` only after its runtime containers are removed. If cleanup fails, the namespace deploy is failed or partially completed using the same phase-promotion rule.
