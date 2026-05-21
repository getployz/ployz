---
title: "ployzctl branch — fork environments"
description: "Fork a namespace into a new environment for PR review, testing, or staging. Control per-resource copy-on-write semantics and apply branches atomically."
llms:
  summary: "Fork a namespace into a new environment for PR review, testing, or staging. Control per-resource copy-on-write semantics and apply branches atomically."
---
The `branch` command forks one namespace into another. It is Ployz's primitive for creating ephemeral environments — PR previews, staging clones, A/B variants — without duplicating data unnecessarily. Branching is a single atomic operation. The source namespace continues to run unaffected.

```bash
ployzctl branch <subcommand> <source> <target> [flags]
```

## Resource modes

When branching a namespace, each resource (service, volume) can be in one of two modes:

| Mode     | Meaning                                                                                                                 |
| -------- | ----------------------------------------------------------------------------------------------------------------------- |
| `branch` | Copy-on-write fork. The resource shares the source's data until it diverges. For volumes, this is an instant ZFS clone. |
| `fresh`  | Empty or newly created resource. The target starts with no data from the source.                                        |

The default is `branch` for services and `fresh` for volumes. You can override the default for all resources of a type, or set per-resource overrides by name.

## Subcommands

### preview — preview what branching would do

  Compute the branch plan and return what would be created, cloned, and configured — without applying anything. Use this to inspect the plan before committing.

  ```bash
  ployzctl branch preview <source> <target> [flags]
  ```

  - `source` (`string`) required:

    The source namespace to fork from.
  

  - `target` (`string`) required:

    The target namespace to create. Must not already exist.
  

  - `--service-mode` (`fresh | branch`):

    Default mode for all services in the branched namespace.
  

  - `--volume-mode` (`fresh | branch`):

    Default mode for all volumes in the branched namespace.
  

  - `--service` (`NAME=MODE`):

    Per-service mode override. Format: `--service myservice=fresh`. Can be specified multiple times.
  

  - `--volume` (`NAME=MODE`):

    Per-volume mode override. Format: `--volume myvolume=branch`. Can be specified multiple times.
  

### prepare — prepare a branch without applying

  Prepare the branch operation and return a prepared deploy ID without executing it. The prepared ID can be stored and applied later with `apply-prepared`. This enables two-phase workflows where the branch is computed in CI but applied from a separate process.

  ```bash
  ployzctl branch prepare <source> <target> [flags]
  ```

  Accepts the same flags as `preview`. The response includes a `prepared_deploy_id` that you pass to `apply-prepared`.

  :::note
    Prepared deploys expire. Check the `expires_at` field in the response and apply before the deadline.
  :::

### apply — apply a branch operation

  Execute the branch operation atomically. The target namespace is created with services and volumes according to the specified resource modes.

  ```bash
  ployzctl branch apply <source> <target> [flags]
  ```

  - `source` (`string`) required:

    The source namespace to fork from.
  

  - `target` (`string`) required:

    The target namespace to create.
  

  - `--service-mode` (`fresh | branch`):

    Default mode for all services.
  

  - `--volume-mode` (`fresh | branch`):

    Default mode for all volumes.
  

  - `--service` (`NAME=MODE`):

    Per-service mode override. Can be specified multiple times.
  

  - `--volume` (`NAME=MODE`):

    Per-volume mode override. Can be specified multiple times.
  

### apply-prepared — apply a previously prepared branch

  Apply a branch operation that was previously computed by `prepare`. The `prepared-deploy-id` identifies the prepared plan.

  ```bash
  ployzctl branch apply-prepared <prepared-deploy-id>
  ```

  - `prepared-deploy-id` (`string`) required:

    The prepared deploy ID returned by `branch prepare`.
  

### render-manifest — render the branch manifest

  Render the TOML manifest that the branch operation would generate, without applying or preparing it. Useful for auditing or storing the manifest in version control.

  ```bash
  ployzctl branch render-manifest <source> <target> [flags]
  ```

  Accepts the same positional arguments and flags as `apply`. Outputs the manifest to stdout.

## PR branching workflow

The following steps show a typical pull request branching workflow, where each PR gets its own isolated environment forked from production.

### Preview the branch plan

  Run `branch preview` to confirm which services and volumes will be cloned versus started fresh.

  ```bash
  ployzctl branch preview production pr-42 \
    --service-mode branch \
    --volume-mode fresh \
    --volume uploads=branch
  ```

### Apply the branch

  When the plan looks correct, apply the branch. The target namespace is created atomically.

  ```bash
  ployzctl branch apply production pr-42 \
    --service-mode branch \
    --volume-mode fresh \
    --volume uploads=branch
  ```

### Deploy the PR build

  Deploy the PR's container image into the new namespace.

  ```bash
  ployzctl deploy service web \
    --image ghcr.io/myorg/web@sha256:newdigest \
    --namespace pr-42
  ```

### Tear down after merge

  When the PR is closed, remove the branch namespace. Use `deploy` with a manifest that removes the services, or drain and remove resources directly.

## Examples

```bash
# Preview what branching production to pr-39 would do
ployzctl branch preview production pr-39

# Apply a branch with default modes (services=branch, volumes=fresh)
ployzctl branch apply production pr-39

# Branch with all volumes cloned (copy-on-write)
ployzctl branch apply production pr-39 --volume-mode branch

# Override a single volume to be fresh while others are branched
ployzctl branch apply production pr-39 \
--volume-mode branch \
--volume tmp-cache=fresh

# Two-phase: prepare in CI, apply from deployment system
PREPARED=$(ployzctl --json branch prepare production pr-39 | jq -r '.prepared_deploy_id')
ployzctl branch apply-prepared "$PREPARED"

# Render the manifest without applying
ployzctl branch render-manifest production pr-39
```
