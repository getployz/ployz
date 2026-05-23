---
title: "Deploying workloads with ployzctl"
description: "Deploy services to a Ployz cluster from a manifest file or inline flags. Covers deploy phases, the commit boundary, dry runs, and building images locally."
llms:
  summary: "Deploy services to a Ployz cluster from a manifest file or inline flags. Covers deploy phases, the commit boundary, dry runs, and building images locally."
---
Ployz deploys are explicit, phase-aware operations. You run `ployzctl deploy`, Ployz inspects the cluster, builds a plan, starts candidate containers, and commits routing facts — in that order. Every step is visible. The deploy either completes cleanly or fails with a clear reason; there is no silent partial state.

## Deploying from a manifest

The primary deploy path reads a manifest file that describes your namespace, services, and volumes:

```bash
ployzctl deploy -f deploy.toml
```

| Flag              | Description                                   |
| ----------------- | --------------------------------------------- |
| `-f`, `--file`    | Path to the manifest file (TOML or JSON)      |
| `-n`, `--dry-run` | Preview the plan without applying any changes |

Run a dry run first to see exactly what Ployz will do before committing anything:

```bash
ployzctl deploy -f deploy.toml --dry-run
```

### Manifest structure

A minimal manifest declares a namespace and at least one service:

```json
{
"namespace": "production",
"services": [
  {
    "name": "web",
    "placement": {
      "replicated": { "count": 2 }
    },
    "template": {
      "image": "myapp:sha256-abc123",
      "env": {
        "PORT": "8080"
      }
    },
    "network": "overlay",
    "service_ports": [
      { "name": "http", "container_port": 8080, "protocol": "tcp" }
    ],
    "readiness": {
      "http": { "service_port": "http", "path": "/healthz" },
      "start_period": "10s"
    },
    "restart": "unless-stopped"
  }
]
}
```

## Previewing a deploy

`deploy preview` computes the plan against current cluster state and prints it without making any changes. It is equivalent to `deploy --dry-run` but available as an explicit subcommand:

```bash
ployzctl deploy preview -f deploy.toml
```

The preview shows participating machines, service changes (create, replace, or unchanged), volume moves, and any warnings about unreachable participants.

## Deploying a service inline

For quick one-off deploys, `deploy service` accepts service parameters directly as flags instead of a manifest file:

```bash
ployzctl deploy service my-api \
--image registry.example.com/my-api:sha256-def456 \
--namespace production \
--publish 8080:8080 \
--env PORT=8080 \
--env LOG_LEVEL=info \
--volume /data:/app/data \
--network overlay \
--pull \
--restart unless-stopped
```

| Flag              | Description                                            |
| ----------------- | ------------------------------------------------------ |
| `name`            | Service name (positional, required)                    |
| `--image`         | Container image reference (required)                   |
| `--namespace`     | Namespace to deploy into (required)                    |
| `-p`, `--publish` | Port mapping in `HOST:CONTAINER` format                |
| `-e`, `--env`     | Environment variable in `KEY=VALUE` format; repeatable |
| `-v`, `--volume`  | Volume mount in `SRC:DST` format; repeatable           |
| `--network`       | Network attachment (default: `overlay`)                |
| `--pull`          | Force pulling the image before starting                |
| `--restart`       | Restart policy (default: `unless-stopped`)             |
| `-n`, `--dry-run` | Preview the service deploy without applying            |

:::tip
Use `--dry-run` with `deploy service` to verify placement and flag any issues before the container starts.
:::

## Deploy phases

Every deploy moves through a fixed sequence of phases:

### Plan

  Ployz reads the manifest, resolves the current cluster state from the store, and builds a placement plan. It checks image availability, probes participant machines for live capacity, and validates that preconditions are met. The deploy fails here — before any mutation — if preconditions are missing.

### Apply

  Candidate containers start on their assigned machines. Volume moves and ZFS transfers happen in phase order. For multi-phase deploys, each phase runs to completion before the next begins. The cluster is in transition during this step.

### Commit

  Ployz appends an immutable deploy commit to the control-plane store. This is the point of no return. Routing facts are published and the gateway and DNS rebuild from the committed state. Traffic switches to the new instances.

### Cleanup

  Old instances are drained and removed. If cleanup fails, the failure is recorded as visible status — it does not roll back the commit or erase the fact that the new version is live.

### The commit boundary

The commit point is where routing flips. Before the first commit, any failure aborts the deploy and the cluster returns to its prior state. After a checkpoint commit, a later failure is reported as `FailedAfterCheckpoint` — the committed facts remain durable and traffic is already on the new version.

:::caution
Cleanup failures after the commit do not indicate a failed deploy. The new version is live. The `FailedAfterCheckpoint` status means old instances may need manual cleanup. Check `ployzctl status` for details.
:::

## Building images locally

If you need to build a container image before deploying, use the `build local` command:

### Dockerfile

  ```bash
  ployzctl build local \
    --method dockerfile \
    --image myapp:latest \
    ./app
  ```

### Railpack

  ```bash
  ployzctl build local \
    --method railpack \
    --image myapp:latest \
    ./app
  ```

| Flag         | Description                                    |
| ------------ | ---------------------------------------------- |
| `--method`   | Build method: `dockerfile` or `railpack`       |
| `--image`    | Target image name and tag (required)           |
| `--platform` | Target platform (e.g. `linux/amd64`)           |
| `CONTEXT`    | Build context directory (positional, required) |

After building, push the image to your cluster machines with `ployzctl image push` before deploying with a `pull: never` policy.

## Next steps

### [Branch and promote](/operations/branch-and-promote)

  Fork a namespace for a PR, then atomically promote it to production.

### [Rollback](/operations/rollback)

  Restore the previous deploy point, including persistent state.
