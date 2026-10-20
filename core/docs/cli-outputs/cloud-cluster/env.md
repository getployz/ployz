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
      "id": "c0416cad-68b4-4d03-98d9-c3daae09f23a",
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
      "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
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
      "id": "f575545e-fcaf-4cbb-bea1-0a10d0dd52eb",
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
      "id": "0b58b231-d9d7-4fe2-9d91-29b92d4d1895",
      "merged": false,
      "name": "staging",
      "parent": "production",
      "protected": false,
      "protections": 0,
      "removal": null
    }
  ],
  "project": {
    "id": "2e071d35-63e2-4eb6-829e-a201a9427ffa",
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
Uses api live from production.
Uses 764d670a-b933-456c-a077-c731b21a41cd live, but nothing runs it.
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
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["api","postgres","web","worker","volumes.pg-data","configs.sentry"]},"message":"No node named pg-data"}}
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
      "id": "a3a2d577-3a83-48cc-86ef-651d1e8f2949",
      "name": "preview",
      "project": "shop",
      "revision": 2
    },
    "kept": false,
    "live": [
      {
        "data": false,
        "name": "api",
        "owner": "production",
        "used_by": [
          "web"
        ]
      },
      {
        "data": false,
        "name": "764d670a-b933-456c-a077-c731b21a41cd",
        "owner": null,
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
sentry/config.yml references redis, which was removed; deploys skip it until you change it: ployz config put sentry config.yml --from PATH --env hotfix
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
  worker (depth 0): switching 1/1
  worker (depth 0): switched 1/1
  Config sentry: completed
────────────────────────────
2 deployed · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### ps in a Branch

```console
$ ployz ps --env hotfix
# exit 0
```

stdout:
```
SERVICE	KIND	SERVER	STATE
worker	service	machine-1	running
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
Uses 764d670a-b933-456c-a077-c731b21a41cd live, but nothing runs it.
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
sentry/config.yml references redis, which was removed; deploys skip it until you change it: ployz config put sentry config.yml --from PATH --env hotfix
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
  api (depth 0): waiting
  worker (depth 1): waiting
  Config sentry: pending
! api-hotfix.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  api (depth 0): booting 0/1
  worker (depth 1): waiting 0/1
! api's image traefik/whoami:v1.11.0 exposes 80; its domains send to 8080. Set the domain port or PORT.
  api (depth 0): switching 1/1
  api (depth 0): switched 1/1
  worker (depth 1): booting 0/1
  worker (depth 1): switching 1/1
  worker (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
2 deployed · 1 unchanged · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

## sync

### env sync --plan to the Parent

```console
$ ployz env sync --env hotfix --to --plan
# exit 0
```

stdout:
```
hotfix → production (version 34:76c98ded01f9e498):
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
    "id": "920939c0-1f49-40f9-9ea1-77325ac9f8d4",
    "name": "hotfix",
    "project": "shop",
    "revision": 4
  },
  "into": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 34
  },
  "never_synced": [],
  "next": "ployz env sync --to --version 34:76c98ded01f9e498 --env hotfix",
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
      "row": "254b1264-6d96-4e34-86d2-795920ddab60:variables.LOG_LEVEL",
      "secret": null,
      "ticked": true
    }
  ],
  "version": "34:76c98ded01f9e498"
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
undo: ployz env sync --undo c608bf28-13f5-4de6-877b-68029da66911
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
sentry/config.yml references redis, which was removed; deploys skip it until you change it: ployz config put sentry config.yml --from PATH
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
    "id": "920939c0-1f49-40f9-9ea1-77325ac9f8d4",
    "name": "hotfix",
    "project": "shop",
    "revision": 5
  },
  "into": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 37
  },
  "next": "ployz deploy --env production",
  "staged": [
    "api"
  ],
  "sync": "8e77a8cf-6b36-4133-841c-c27b3de25eb8"
}
```

### env sync --from the Parent

```console
$ ployz env sync --env fix-api --from production --plan
# exit 0
```

stdout:
```
production → fix-api (version 12:b580324783b24b95):
  nothing to sync
```

### env sync --plan of the seeded fix-api Branch

```console
$ ployz env sync --env fix-api --to --plan
# exit 0
```

stdout:
```
fix-api → production (version 38:8ecbfd252e413cdd):
  api.env.LOG_LEVEL: "warn" → "debug"
  web.startCommand: null → "npm run serve" (new)
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

### env rm

```console
$ ployz env rm hotfix --confirm shop/hotfix
# exit 124
```

stderr:
```
Removing shop/hotfix removes Service api, removes Service worker.
This removal destroys 2 things:
  ✕ removes Service api
  ✕ removes Service worker
Approve 62f502bd-7ee2-4555-afd0-420cbe8213bf in Ployz Cloud at http://localhost:38265
next: ployz org approval approve 62f502bd-7ee2-4555-afd0-420cbe8213bf
Waiting for approval in Ployz Cloud… 
Timed out after 300s
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
    "id": "a3a2d577-3a83-48cc-86ef-651d1e8f2949",
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
hotfix	-	-	production	-	-
production	default	yes	-	-	-
search	-	-	staging	-	-
staging	-	-	production	-	-
```

