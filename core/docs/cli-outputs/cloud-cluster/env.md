# Environments and Branches through Cloud (deployed Parent)

## ls

### env ls

```console
$ ployz env ls
# exit 0
```

stdout:
```
ENVIRONMENT	DEFAULT	PROTECTED	BRANCH OF	REMOVAL	BRANCH SETUP
fix-api	-	-	production	-	-
production	default	yes	-	-	-
search	-	-	staging	-	-
staging	-	-	production	-	-
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
      "id": "977b82b1-8c32-4ed4-be58-acab8591d655",
      "merged": false,
      "name": "fix-api",
      "parent": "production",
      "protected": false,
      "protections": 0,
      "removal": null
    },
    {
      "branch_setup": [],
      "default": true,
      "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
      "merged": false,
      "name": "production",
      "parent": null,
      "protected": true,
      "protections": 1,
      "removal": null
    },
    {
      "branch_setup": [],
      "default": false,
      "id": "e06b555a-c5b8-471e-bd69-be1744b84348",
      "merged": false,
      "name": "search",
      "parent": "staging",
      "protected": false,
      "protections": 0,
      "removal": null
    },
    {
      "branch_setup": [],
      "default": false,
      "id": "0aa4e2fa-8202-458d-8cad-77dc33367fa6",
      "merged": false,
      "name": "staging",
      "parent": "production",
      "protected": false,
      "protections": 0,
      "removal": null
    }
  ],
  "project": {
    "id": "41713a61-0fda-483f-b93e-9aa12c438625",
    "name": "shop"
  }
}
```

## branch

### env branch --live of a node nothing copied uses (refused)

```console
$ ployz env branch hotfix --live postgres --copy api
# exit 2
```

stderr:
```
error: postgres can't be used live: nothing the Branch copies uses it
```

### env branch --json --live of a node nothing copied uses (refused)

```console
$ ployz --json env branch hotfix --live postgres --copy api
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":{"node":"postgres"},"message":"postgres can't be used live: nothing the Branch copies uses it"}}
```

### env branch --copy (api, which worker references, stays live)

```console
$ ployz env branch hotfix --copy worker
# exit 0
```

stdout:
```
Made Branch shop/hotfix of production.
Staged: worker, configs.sentry
Uses redis live from production.
Uses api live from production.
```

stderr:
```
next: ployz deploy --env hotfix
```

### env branch --json --copy --live (a Volume by its bare name, refused)

```console
$ ployz --json env branch preview --copy postgres --live pg-data
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["web","postgres","redis","worker","api","volumes.pg-data","configs.sentry"]},"message":"No node named pg-data"}}
```

### env branch --json --live of a Volume the Branch copies (refused)

```console
$ ployz --json env branch preview --copy postgres --live volumes.pg-data
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":{"node":"volumes.pg-data"},"message":"volumes.pg-data can't be used live: the Branch copies it, so nothing running can lend it"}}
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
      "id": "18953e68-a474-4236-aa42-f2f9777d1112",
      "name": "preview",
      "project": "shop",
      "revision": 2
    },
    "kept": false,
    "live": [
      {
        "data": false,
        "name": "redis",
        "owner": "production",
        "used_by": [
          "web"
        ]
      },
      {
        "data": false,
        "name": "api",
        "owner": "production",
        "used_by": [
          "web"
        ]
      }
    ],
    "parent": "production",
    "pull_request": null,
    "setup": [],
    "to_parent": 0
  },
  "next": "ployz deploy --env preview",
  "staged": [
    "web",
    "configs.sentry"
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
ENVIRONMENT	DEFAULT	PROTECTED	BRANCH OF	REMOVAL	BRANCH SETUP
fix-api	-	-	production	-	-
hotfix	-	-	production	-	-
preview	-	-	production	-	-
production	default	yes	-	-	-
search	-	-	staging	-	-
staging	-	-	production	-	-
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
  worker.image: null → "traefik/whoami:v1.10.3"
sentry (create)
```

stderr:
```
next: ployz deploy --env hotfix
```

### deploy the Branch

```console
$ ployz deploy --env hotfix
# exit 0
```

stderr:
```
Deploying #1 of shop/hotfix
  worker (depth 0): waiting 0/1
  Config sentry: pending
  worker (depth 0): booting 0/1
  worker (depth 0): switched 1/1
  Config sentry: completed
────────────────────────────
2 deployed · across 1 server
```

### ps in a Branch

```console
$ ployz ps --env hotfix
# exit 0
```

stdout:
```
SERVICE	KIND	SERVER	STATE
worker	service	machine-1	running, healthy
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
Uses redis live from production.
```

stderr:
```
next: ployz deploy --env hotfix
```

### diff after env copy

```console
$ ployz diff --env hotfix
# exit 0
```

stdout:
```
api (create)
  api.image: null → "traefik/whoami:v1.11.0"
```

stderr:
```
next: ployz deploy --env hotfix
```

### env copy --json a Volume (refused: takes a Service)

```console
$ ployz --json env copy volumes.pg-data --env hotfix
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"Expected a Service name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit, like web"}}
```

### env copy --json a Live Node, already copied

```console
$ ployz --json env copy api --env hotfix
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"conflict","details":{"node":"api"},"message":"This Branch already has its own copy of api"}}
```

### env copy a node that is not live

```console
$ ployz env copy worker --env hotfix
# exit 1
```

stderr:
```
error: This Branch already has its own copy of worker
```

### deploy the Branch after copy

```console
$ ployz deploy --env hotfix
# exit 0
```

stderr:
```
Deploying #2 of shop/hotfix
  api (depth 0): booting 0/1
  worker (depth 1): waiting 0/1
  Config sentry: pending
  api (depth 0): switching 1/1
  api (depth 0): switched 1/1
  worker (depth 1): booting 0/1
  worker (depth 1): switching 1/1
  worker (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
2 deployed · 1 unchanged · across 1 server
```

## sync

### env sync --plan to the Parent

```console
$ ployz env sync --env hotfix --to --plan
# exit 0
```

stdout:
```
hotfix → production (version 35:2a5855067ef59939):
  api (update)
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
  "from": {
    "id": "3ce33c5a-17f5-4497-85d5-230e6e734f96",
    "name": "hotfix",
    "project": "shop",
    "revision": 4
  },
  "into": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 35
  },
  "never_synced": [],
  "next": "ployz env sync --to --version 35:2a5855067ef59939 --env hotfix",
  "proposal": null,
  "rows": [
    {
      "change": "changed",
      "from": "info",
      "held_by": null,
      "into": "warn",
      "kind": "service",
      "name": "env.LOG_LEVEL",
      "node": "api",
      "requires": null,
      "row": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d:variables.LOG_LEVEL",
      "secret": null,
      "ticked": true
    }
  ],
  "version": "35:2a5855067ef59939"
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
Staged: api
```

stderr:
```
undo: ployz env sync --undo 52c83edc-197f-4cac-9aa6-c40bddd67e1e
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
  api.env.LOG_LEVEL: "warn" → "info"
```

stderr:
```
next: ployz deploy
```

### discard in production

```console
$ ployz discard
# exit 0
```

stdout:
```
Discarded 1 staged change in shop/production: api.env.LOG_LEVEL.
```

stderr:
```
inspect: ployz diff
```

### env sync --json to the Parent

```console
$ ployz --json env sync --env hotfix --to
# exit 0
```

stdout:
```
{
  "closing": false,
  "from": {
    "id": "3ce33c5a-17f5-4497-85d5-230e6e734f96",
    "name": "hotfix",
    "project": "shop",
    "revision": 5
  },
  "into": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 38
  },
  "next": "ployz deploy --env production",
  "staged": [
    "api"
  ],
  "sync": "d5f579ee-1711-455a-98f2-85fabba30184"
}
```

### env sync --from the Parent

```console
$ ployz env sync --env fix-api --from production --plan
# exit 0
```

stdout:
```
production → fix-api (version 12:f546a9816d2b7ab7):
  nothing to sync
```

### env sync --plan of the seeded fix-api Branch

```console
$ ployz env sync --env fix-api --to --plan
# exit 0
```

stdout:
```
fix-api → production (version 39:70df6a3024ad7d95):
  web (update)
    web.startCommand: null → "npm run serve" (new)
  api (update)
    api.env.LOG_LEVEL: "warn" → "debug"
```

## rm

### env rm without --confirm

```console
$ ployz env rm hotfix
# exit 2
```

stderr:
```
error: Removing Environment hotfix deletes its configuration, history and every Service and Volume in it; this can't be undone. No changes made.
retry: ployz env rm hotfix --confirm shop/hotfix
```

### env rm (Ada approves it)

```console
$ ployz env rm hotfix --confirm shop/hotfix
# exit 0
```

stdout:
```
Removed Environment shop/hotfix.
```

stderr:
```
Removing shop/hotfix removes Service api, removes Service worker.
This removal destroys 2 things:
  ✕ removes Service api
  ✕ removes Service worker
Approve 4cffce01-846a-472e-a908-93244a560ce3 in Ployz Cloud at http://localhost:33907
next: ployz org approval approve 4cffce01-846a-472e-a908-93244a560ce3
Waiting for approval in Ployz Cloud… approved.
Deploying #3 of shop/hotfix
  api: removing 0/1
  worker: waiting to remove 0/1
  Config sentry: pending
  api: removed 1/1
  worker: removing 0/1
  worker: removed 1/1
```

### env rm --json

```console
$ ployz --json env rm preview --confirm shop/preview
# exit 0
```

stdout:
```
{
  "deployments": [],
  "environment": {
    "id": "18953e68-a474-4236-aa42-f2f9777d1112",
    "name": "preview",
    "project": "shop",
    "revision": 2
  },
  "removal": "removed",
  "residuals": []
}
```

### env ls after

```console
$ ployz env ls
# exit 0
```

stdout:
```
ENVIRONMENT	DEFAULT	PROTECTED	BRANCH OF	REMOVAL	BRANCH SETUP
fix-api	-	-	production	-	-
production	default	yes	-	-	-
search	-	-	staging	-	-
staging	-	-	production	-	-
```

