---
title: "Branching and promoting environments"
description: "Fork a namespace atomically for a PR or staging environment, then promote it to production. Covers branch modes, prepare/apply-prepared, and lineage."
llms:
  summary: "Fork a namespace atomically for a PR or staging environment, then promote it to production. Covers branch modes, prepare/apply-prepared, and lineage."
---
Branching lets you fork an entire namespace — services, volumes, routing — into a new environment as a single atomic operation. The typical use case is creating an isolated environment for a pull request, then promoting it to production when the PR is approved. Every branch lineage fact is committed to the control plane as durable evidence of where each service revision came from.

## How branching works

When you branch a namespace, Ployz copies service configurations and optionally volumes from a source namespace into a new target namespace. Each resource can be either **branched** (copied from the source) or **fresh** (created independently in the target):

* `Branch` — the target resource starts from the source's current revision or volume state.
* `Fresh` — the target resource is created independently, with no lineage link to the source.

The default behavior is `Branch` for services (so they inherit the source's container spec) and `Fresh` for volumes (so the PR environment gets a clean data set rather than a live copy of production data).

## Branch subcommands

| Subcommand              | What it does                                                               |
| ----------------------- | -------------------------------------------------------------------------- |
| `branch preview`        | Computes the branch plan and prints it without making any changes          |
| `branch prepare`        | Resolves the plan and saves it as a prepared deploy, but does not apply it |
| `branch apply`          | Previews, plans, and applies in one step                                   |
| `branch apply-prepared` | Applies a previously prepared branch deploy by its ID                      |

## Flags

All branch subcommands (except `apply-prepared`) accept the same arguments:

```bash
ployzctl branch preview <source-namespace> <target-namespace> [flags]
ployzctl branch prepare <source-namespace> <target-namespace> [flags]
ployzctl branch apply   <source-namespace> <target-namespace> [flags]
```

| Flag                  | Default  | Description                                    |
| --------------------- | -------- | ---------------------------------------------- |
| `--service-mode`      | `branch` | Default mode for services: `branch` or `fresh` |
| `--volume-mode`       | `fresh`  | Default mode for volumes: `branch` or `fresh`  |
| `--service NAME=MODE` | —        | Per-service mode override; repeatable          |
| `--volume NAME=MODE`  | —        | Per-volume mode override; repeatable           |

### Per-resource overrides

Use `--service` and `--volume` to override the mode for individual resources:

```bash
ployzctl branch apply production pr-42 \
--service-mode branch \
--volume-mode fresh \
--volume db=branch \
--service worker=fresh
```

In this example, the `db` volume is cloned from production (copy-on-write, instant), while the `worker` service gets a fresh empty instance instead of inheriting the production spec.

## Typical workflow: preview → prepare → apply-prepared

For production environments, use the three-step workflow to separate the planning phase from the apply phase. This lets you inspect the plan and apply it later — for example, after a CI check passes — without the plan becoming stale:

### Preview the branch

  Compute the plan and verify what will be created in the target namespace.

  ```bash
  ployzctl branch preview production pr-42 \
    --service-mode branch \
    --volume-mode fresh
  ```

  The preview shows which services will be created, which volumes will be cloned or freshly provisioned, and which machines will participate.

### Prepare the branch

  Resolve and save the plan as a prepared deploy. The command prints the `prepared_deploy_id`.

  ```bash
  ployzctl branch prepare production pr-42 \
    --service-mode branch \
    --volume-mode fresh
  ```

  The prepared deploy is stored in the control plane and has a TTL. It will expire if not applied within the configured window.

### Apply the prepared deploy

  Apply the saved plan by its ID. This step acquires the deploy lock and commits the facts.

  ```bash
  ployzctl branch apply-prepared <prepared-deploy-id>
  ```

  Once applied, the target namespace is live with its own routing, DNS entries, and gateway rules.

:::note
Branch lineage is committed as a durable fact in the control plane. After a branch is applied, you can always trace which source namespace and source revision a branched service came from. Raw manifests are not stored as evidence, but the lineage records are.
:::

## Promoting to production

Promotion atomically switches traffic from one namespace to another. It is implemented through the deploy path: you apply a manifest in the target namespace that references the branched services, and the routing commit point flips traffic.

```bash
# After validating the PR environment, promote by deploying the
# branched service spec back into production
ployzctl deploy -f promote.toml
```

The old production namespace remains snapshotted by ZFS until you explicitly remove it, giving you an instant rollback point.

:::tip
Preview the promotion deploy with `--dry-run` before applying. The preview will show routing changes and confirm which machines will be involved in the commit.
:::

## Resource mode reference

| Mode     | Services                                                                               | Volumes                                                              |
| -------- | -------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| `Branch` | Target service inherits the source's container spec and revision. Lineage is recorded. | Target volume is a ZFS clone of the source — instant, copy-on-write. |
| `Fresh`  | Target service is created independently with no source lineage.                        | Target volume is provisioned as a new empty dataset.                 |

:::caution
Branching a volume with `Branch` mode clones the source volume's data at apply time. Changes in the branch environment do not affect the source. Changes in the source after the clone do not appear in the branch.
:::

## Next steps

### [Deploy](/operations/deploy)

  Understand the deploy phases and commit model that powers branch and promote.

### [Rollback](/operations/rollback)

  Restore a previous deploy point if a promotion goes wrong.
