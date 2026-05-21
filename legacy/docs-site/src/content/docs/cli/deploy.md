---
title: "ployzctl deploy — deploy workloads"
description: "Apply deploy manifests, preview changes before committing, or deploy a single service inline. Includes dry-run support and the full service flag reference."
llms:
  summary: "Apply deploy manifests, preview changes before committing, or deploy a single service inline. Includes dry-run support and the full service flag reference."
---
The `deploy` command applies workload definitions to the cluster. It supports three modes: applying a manifest file, previewing a manifest without committing, or deploying a single service inline without a file. Every deploy operation has a bounded effect and returns a structured result.

```bash
ployzctl deploy [flags]
ployzctl deploy <subcommand> [flags]
```

:::note
When no subcommand is given, `deploy` applies the manifest. Use `deploy preview` to inspect what would change before committing, especially in production.
:::

## Apply a manifest (default)

Apply a deploy manifest to the cluster. The manifest is read from a file (or stdin if `-f -` is used) and the daemon validates, plans, and executes the deploy.

```bash
ployzctl deploy [-f <path>] [-n]
```

- `-f / --file` (`PATH`):

Path to the deploy manifest file. If omitted, the daemon looks for a default manifest in the current directory. Pass `-f -` to read from stdin.

- `-n / --dry-run` (`boolean`):

Validate and plan the deploy without executing it. The daemon returns what would change, but no containers are started or stopped.

### Sample deploy manifest

Deploy manifests are written in TOML. A minimal manifest for a single service looks like this:

```toml
[namespace]
name = "production"

[[service]]
name = "web"
image = "ghcr.io/myorg/web@sha256:abc123..."
network = "overlay"
restart = "unless-stopped"

[[service.publish]]
host = 80
container = 8080

[[service.env]]
key = "DATABASE_URL"
value = "postgres://..."
```

:::tip
Pin images to a digest (`image@sha256:...`) rather than a mutable tag. The daemon requires a digest for deploy image availability checks in many configurations.
:::

## Subcommands

### preview — preview a manifest without applying

  Compute what a deploy would do and return the plan without executing it. Useful for CI pipelines and code review workflows.

  ```bash
  ployzctl deploy preview [-f <path>] [-n]
  ```

  - `-f / --file` (`PATH`):

    Path to the deploy manifest to preview. Behavior is the same as for the default apply action.
  

  - `-n / --dry-run` (`boolean`):

    Dry-run flag. When combined with `preview`, no state is read or computed beyond what is needed for validation.
  

  The preview response includes the namespace, manifest hash, placement plan, service sources, image availability records, volume moves, and any warnings — without committing anything.

### service — deploy a single service inline

  Deploy a single named service without a manifest file. All configuration is specified as command-line flags. This is useful for quick iterations, bootstrapping, and scripts that construct service definitions programmatically.

  ```bash
  ployzctl deploy service <name> --image <image> --namespace <namespace> [flags] [-- command...]
  ```

  **Positional arguments**

  - `name` (`string`) required:

    The name of the service to deploy.
  

  **Required flags**

  - `--image` (`string`) required:

    Container image reference for the service, e.g. `ghcr.io/myorg/app@sha256:abc123`.
  

  - `--namespace` (`string`) required:

    The namespace to deploy the service into. Namespaces isolate workloads and are the unit of branching.
  

  **Optional flags**

  - `-p / --publish` (`HOST:CONTAINER`):

    Publish a port. The format is `HOST:CONTAINER`, e.g. `-p 80:8080`. Can be specified multiple times.
  

  - `-e / --env` (`KEY=VALUE`):

    Set an environment variable. The format is `KEY=VALUE`, e.g. `-e DATABASE_URL=postgres://...`. Can be specified multiple times.
  

  - `-v / --volume` (`SRC:DST`):

    Mount a volume. The format is `SRC:DST`, e.g. `-v mydata:/data`. Can be specified multiple times.
  

  - `--network` (`string`):

    Network to attach the service to.
  

  - `--pull` (`boolean`):

    Force a fresh image pull before starting the container.
  

  - `--restart` (`string`):

    Container restart policy. Common values: `unless-stopped`, `always`, `on-failure`, `no`.
  

  - `-n / --dry-run` (`boolean`):

    Validate and plan the service deploy without executing it.
  

  **Command override**

  Arguments after `--` are passed as the container command, overriding the image's default `CMD`.

  ```bash
  ployzctl deploy service worker \
    --image ghcr.io/myorg/app@sha256:abc123 \
    --namespace production \
    -- python worker.py --queue high
  ```

## Examples

```bash
# Apply a manifest from the default location
ployzctl deploy

# Apply a specific manifest file
ployzctl deploy -f ployz.toml

# Dry-run to see what would change
ployzctl deploy -f ployz.toml -n

# Preview a manifest and get structured JSON output
ployzctl --json deploy preview -f ployz.toml

# Deploy a single service inline
ployzctl deploy service web \
--image ghcr.io/myorg/web@sha256:abc123 \
--namespace production \
-p 80:8080 \
-e APP_ENV=production

# Deploy with a custom restart policy and volume
ployzctl deploy service db \
--image postgres:16 \
--namespace staging \
-p 5432:5432 \
-v pgdata:/var/lib/postgresql/data \
-e POSTGRES_PASSWORD=secret \
--restart always
```

:::caution
In production, always run `ployzctl deploy preview` before `ployzctl deploy` when updating services that have persistent volumes. The preview shows volume moves, which are non-trivial operations.
:::
