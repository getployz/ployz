# Environments and Branches through Cloud (deployed Parent)

## ls

### env ls

```console
$ ployz env ls
# exit 0
```

stdout:
```
Environments of Project shop:
  fix-api (branch of production)
  production (default)
```

### env ls --json

```console
$ ployz --json env ls
# exit 0
```

stdout:
```
{
  "environments": [
    {
      "branch_setup": [],
      "default": false,
      "id": "f8a9a59e-5855-42cd-a5c4-cc7737692ef8",
      "name": "fix-api",
      "parent": "production",
      "removal": null
    },
    {
      "branch_setup": [],
      "default": true,
      "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
      "name": "production",
      "parent": null,
      "removal": null
    }
  ],
  "project": {
    "id": "930a7689-a60c-42a6-8e9f-d06bc337d8d8",
    "name": "shop"
  }
}
```

## branch

### env branch --live of a node nothing copied uses (refused)

```console
$ ployz env branch hotfix --live postgres --copy api
# exit 1
```

stderr:
```
postgres can't be used live: nothing the Branch copies uses it
```

### env branch --json --live of a node nothing copied uses (refused)

```console
$ ployz --json env branch hotfix --live postgres --copy api
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":{"node":"postgres"},"message":"postgres can't be used live: nothing the Branch copies uses it"}}
```

### env branch --copy (api, which worker references, stays live)

```console
$ ployz env branch hotfix --copy worker
# exit 0
```

stdout:
```
Made Branch shop/hotfix of production.
Staged: worker
Uses api live from production.
```

### env branch --json --copy --live (a Volume by its bare name, refused)

```console
$ ployz --json env branch preview --copy postgres --live pg-data
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"did_you_mean":null,"valid_children":["web","api","postgres","worker","volumes.pg-data"]},"message":"No node named pg-data"}}
```

### env branch --json --live of a Volume the Branch copies (refused)

```console
$ ployz --json env branch preview --copy postgres --live volumes.pg-data
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":{"node":"volumes.pg-data"},"message":"volumes.pg-data can't be used live: the Branch copies it: nothing running can lend it"}}
```

### env branch --json --copy

```console
$ ployz --json env branch preview --copy web
# exit 0
```

stdout:
```
{
  "branch": {
    "closes_at": null,
    "environment": {
      "id": "f2c12198-f5f9-4662-9242-d11752d21f4d",
      "name": "preview",
      "project": "shop",
      "revision": 2
    },
    "kept": false,
    "live": [],
    "parent": "production",
    "pull_request": null,
    "setup": [],
    "to_parent": 0
  },
  "next": "ployz deploy --env preview",
  "staged": [
    "web"
  ]
}
```

### env ls with Branches

```console
$ ployz env ls
# exit 0
```

stdout:
```
Environments of Project shop:
  fix-api (branch of production)
  hotfix (branch of production)
  preview (branch of production)
  production (default)
```

### service ls in a Branch

```console
$ ployz service ls --env hotfix
# exit 0
```

stdout:
```
SERVICE	PRIVATE DNS	SOURCE	NEXT DEPLOY
worker	worker	image	create
```

### diff in a Branch

```console
$ ployz diff --env hotfix
# exit 0
```

stdout:
```
worker (create)
next: ployz deploy --expect-version 2:0:0.0 --env hotfix
```

### deploy the Branch

```console
$ ployz deploy --env hotfix
# exit 0
```

stdout:
```
Following Deployment #1; stopping this leaves it running.
queued: worker pending
running: worker pending
applied: worker deployed
Deployment #1 of shop/hotfix: applied
  worker: deployed
```

### ps in a Branch

```console
$ ployz ps --env hotfix
# exit 0
```

stdout:
```
CONTAINER ID	SERVICE	KIND	MACHINE	STATE
3429a4da7178	worker	service	7784f1a17df144728819cecf47282904	running
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

## copy

### env copy a Live Node

```console
$ ployz env copy api --env hotfix
# exit 0
```

stdout:
```
Copied into Branch shop/hotfix of production.
Staged: api
```

### diff after env copy

```console
$ ployz diff --env hotfix
# exit 0
```

stdout:
```
api (create)
next: ployz deploy --expect-version 3:1:0.1 --env hotfix
```

### env copy --json a Volume (refused: takes a Service)

```console
$ ployz --json env copy volumes.pg-data --env hotfix
# exit 2
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"Expected a Service name: up to 63 lowercase letters, digits and -, like web"}}
```

### env copy --json a Live Node, already copied

```console
$ ployz --json env copy api --env hotfix
# exit 1
```

stdout:
```
{"error":{"code":"conflict","details":{"node":"api"},"message":"This Branch already has its own copy of api"}}
```

### env copy a node that is not live

```console
$ ployz env copy worker --env hotfix
# exit 1
```

stderr:
```
This Branch already has its own copy of worker
```

### deploy the Branch after copy

```console
$ ployz deploy --env hotfix
# exit 0
```

stdout:
```
Following Deployment #2; stopping this leaves it running.
queued: api pending, worker pending
running: api pending, worker pending
running: api deployed, worker pending
applied: api deployed, worker deployed
Deployment #2 of shop/hotfix: applied
  api: deployed
  worker: deployed
```

## sync

### env sync --plan to the Parent

```console
$ ployz env sync --env hotfix --to --plan
# exit 0
```

stdout:
```
hotfix → production (version 63:919a99f91d81b229):
  api.env.LOG_LEVEL: "warn" → "info"
```

### env sync --plan --json to the Parent

```console
$ ployz --json env sync --env hotfix --to --plan
# exit 0
```

stdout:
```
{
  "at_merge": null,
  "from": {
    "id": "c192f7dc-7d3c-4785-b03c-039ae06eed88",
    "name": "hotfix",
    "project": "shop",
    "revision": 4
  },
  "into": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 63
  },
  "never_synced": [],
  "next": "ployz env sync --to --version 63:919a99f91d81b229 --env hotfix",
  "rows": [
    {
      "change": "changed",
      "from": "info",
      "into": "warn",
      "kind": "service",
      "name": "env.LOG_LEVEL",
      "node": "api",
      "requires": null,
      "row": "a48be9de-5046-4ce1-ac09-736234a707d0:variables.LOG_LEVEL",
      "secret": null,
      "ticked": true
    }
  ],
  "version": "63:919a99f91d81b229"
}
```

### env sync to the Parent

```console
$ ployz env sync --env hotfix --to
# exit 0
```

stdout:
```
Synced hotfix → shop/production.
Undo it: ployz env sync --undo 707ce513-d62d-46bb-82c6-b9f8b6c1ef1e
Staged: api
next: ployz deploy --env production
```

### diff in production after sync

```console
$ ployz diff
# exit 0
```

stdout:
```
api (update)
  api.env.LOG_LEVEL: "warn" -> "info"
next: ployz deploy --expect-version 64:36:0.63
```

### discard in production

```console
$ ployz discard
# exit 0
```

stdout:
```
Discarded every staged change in shop/production (revision 65).
```

### env sync --json to the Parent

```console
$ ployz --json env sync --env hotfix --to
# exit 0
```

stdout:
```
{
  "from": {
    "id": "c192f7dc-7d3c-4785-b03c-039ae06eed88",
    "name": "hotfix",
    "project": "shop",
    "revision": 5
  },
  "into": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 66
  },
  "next": "ployz deploy --env production",
  "sync": "a025274c-4e4f-4c45-ba83-29174737a787",
  "when": {
    "closing": false,
    "kind": "now",
    "staged": [
      "api"
    ]
  }
}
```

### env sync --from the Parent

```console
$ ployz env sync --env fix-api --from production --plan
# exit 0
```

stdout:
```
production → fix-api (version 23:b9fa0dd725038ced):
  nothing to sync
```

### env sync --plan of the seeded fix-api Branch

```console
$ ployz env sync --env fix-api --to --plan
# exit 0
```

stdout:
```
fix-api → production (version 67:52e59e21cafbda4f):
  web.startCommand: null → "npm run serve" (new)
  api.env.LOG_LEVEL: "warn" → "debug"
```

## rm

### env rm without --confirm

```console
$ ployz env rm hotfix
# exit 1
```

stderr:
```
Removing Environment hotfix deletes its configuration, history and every Service and Volume in it; this can't be undone. No changes made.
Retry: ployz env rm hotfix --confirm shop/hotfix
```

### env rm

```console
$ ployz env rm hotfix --confirm shop/hotfix
# exit 0
```

stdout:
```
Following Deployment #3; stopping this leaves it running.
queued: api pending, worker pending
running: api pending, worker pending
applied: api removed, worker removed
Removed shop/hotfix from the Servers (Deployment #3).
Removed Environment shop/hotfix.
```

### env rm --json

```console
$ ployz --json env rm preview --confirm shop/preview
# exit 0
```

stdout:
```
{
  "deployment": null,
  "environment": {
    "id": "f2c12198-f5f9-4662-9242-d11752d21f4d",
    "name": "preview",
    "project": "shop",
    "revision": 2
  }
}
```

### env ls after

```console
$ ployz env ls
# exit 0
```

stdout:
```
Environments of Project shop:
  fix-api (branch of production)
  production (default)
```

