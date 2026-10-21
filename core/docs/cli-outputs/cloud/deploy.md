# Ployz Cloud CLI outputs: deploy

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## deployment (seed: Deployment 1 queued; no Server answers)

### deployment ls

```console
$ ployz deployment ls
# exit 0
```

stdout:
```
DEPLOYMENT	STATUS	SAVED REVISION
#1	queued	1
```

### deployment ls --json

```console
$ ployz deployment ls --json
# exit 0
```

stdout:
```
{
  "deployments": [
    {
      "admitted_at": 1792494100,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": null,
      "environment_id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
      "held": false,
      "id": "05fd264c-552d-4cbc-89fa-d8b0106636a2",
      "in_flight": true,
      "message": "Ship everything",
      "number": 1,
      "outcome": null,
      "push": null,
      "remove": false,
      "runner": null,
      "saved": 1,
      "services": [],
      "ships": [
        "redis",
        "postgres",
        "worker",
        "api",
        "web"
      ],
      "started_at": null,
      "status": "queued",
      "upload": null
    }
  ],
  "environment": {
    "id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "in_flight": [
    {
      "admitted_at": 1792494100,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": null,
      "environment_id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
      "held": false,
      "id": "05fd264c-552d-4cbc-89fa-d8b0106636a2",
      "in_flight": true,
      "message": "Ship everything",
      "number": 1,
      "outcome": null,
      "push": null,
      "remove": false,
      "runner": null,
      "saved": 1,
      "services": [],
      "ships": [
        "redis",
        "postgres",
        "worker",
        "api",
        "web"
      ],
      "started_at": null,
      "status": "queued",
      "upload": null
    }
  ],
  "latest": {
    "admitted_at": 1792494100,
    "admitted_by": "Ada Lovelace",
    "ci": null,
    "ended_at": null,
    "environment_id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
    "held": false,
    "id": "05fd264c-552d-4cbc-89fa-d8b0106636a2",
    "in_flight": true,
    "message": "Ship everything",
    "number": 1,
    "outcome": null,
    "push": null,
    "remove": false,
    "runner": null,
    "saved": 1,
    "services": [],
    "ships": [
      "redis",
      "postgres",
      "worker",
      "api",
      "web"
    ],
    "started_at": null,
    "status": "queued",
    "upload": null
  },
  "next_cursor": null
}
```

### deployment show 1

```console
$ ployz deployment show 1
# exit 0
```

stdout:
```
Deployment #1 of shop/production: queued
  redis: pending
  postgres: pending
  worker: pending
  api: pending
  web: pending
  pg-data: pending
  sentry: pending
```

### deployment show 1 --json

```console
$ ployz deployment show 1 --json
# exit 0
```

stdout:
```
{
  "admitted_at": 1792494100,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": null,
  "environment": {
    "id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "environment_id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
  "held": false,
  "id": "05fd264c-552d-4cbc-89fa-d8b0106636a2",
  "in_flight": true,
  "message": "Ship everything",
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "030ddc04-33d9-4149-a489-ee4788da4aa6",
      "name": "redis",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "54b62342-54f6-46c0-aa79-eaa89c03f4bd",
      "name": "postgres",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "99e8f125-0dd3-4a14-baa8-8244c36e8cd9",
      "name": "worker",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "d25db7f1-82e7-4e78-ad08-a12f91c227d2",
      "name": "api",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "f59287b3-06ba-4d82-aa22-f3ff607bc95f",
      "name": "web",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "313a7879-5239-4f45-8d0a-978eb58a8994",
      "name": "pg-data",
      "outcome": "pending",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "46b22ea1-47ad-4f35-baeb-4d044871c79c",
      "name": "sentry",
      "outcome": "pending",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 1,
  "outcome": null,
  "preview": null,
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": null,
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 1,
  "services": [],
  "ships": [
    "redis",
    "postgres",
    "worker",
    "api",
    "web"
  ],
  "started_at": null,
  "status": "queued",
  "upload": null
}
```

### deployment show unknown number

```console
$ ployz deployment show 99
# exit 1
```

stderr:
```
error: shop/production has no Deployment #99
next: ployz deployment ls
```

### deployment show unknown number --json

```console
$ ployz deployment show 99 --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"next":"ployz deployment ls"},"message":"shop/production has no Deployment #99"}}
```

### deployment show garbage id

```console
$ ployz deployment show not-an-id
# exit 2
```

stderr:
```
error: Expected a Deployment ID or number
```

### deployment start 1 --detach

```console
$ ployz deployment start 1 --detach
# exit 0
```

stdout:
```
Deployment #1 of shop/production: queued
  redis: pending
  postgres: pending
  worker: pending
  api: pending
  web: pending
  pg-data: pending
  sentry: pending
```

### deployment start 1 (follows; no Server enrolled)

```console
$ ployz deployment start 1
# exit 1
```

stderr:
```
error: This Organization has no servers yet, so nothing deployed. Add one, then deploy again.
next: ployz server add
```

### deployment cancel 1

```console
$ ployz deployment cancel 1
# exit 0
```

stdout:
```
Deployment #1 of shop/production: cancelled
  redis: not attempted
  postgres: not attempted
  worker: not attempted
  api: not attempted
  web: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment cancel 1 again --json

```console
$ ployz deployment cancel 1 --json
# exit 0
```

stdout:
```
{
  "admitted_at": 1792494100,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792494283,
  "environment": {
    "id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "environment_id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
  "held": false,
  "id": "05fd264c-552d-4cbc-89fa-d8b0106636a2",
  "in_flight": false,
  "message": "Ship everything",
  "namespace": "shop-production",
  "next": "ployz deployment show 1 --project shop",
  "nodes": [
    {
      "id": "030ddc04-33d9-4149-a489-ee4788da4aa6",
      "name": "redis",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "54b62342-54f6-46c0-aa79-eaa89c03f4bd",
      "name": "postgres",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "99e8f125-0dd3-4a14-baa8-8244c36e8cd9",
      "name": "worker",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "d25db7f1-82e7-4e78-ad08-a12f91c227d2",
      "name": "api",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "f59287b3-06ba-4d82-aa22-f3ff607bc95f",
      "name": "web",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "313a7879-5239-4f45-8d0a-978eb58a8994",
      "name": "pg-data",
      "outcome": "not_attempted",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "46b22ea1-47ad-4f35-baeb-4d044871c79c",
      "name": "sentry",
      "outcome": "not_attempted",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 1,
  "outcome": null,
  "preview": null,
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": null,
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 1,
  "services": [],
  "ships": [
    "redis",
    "postgres",
    "worker",
    "api",
    "web"
  ],
  "started_at": null,
  "status": "cancelled",
  "upload": null
}
```

### deployment redeploy 1 --detach

```console
$ ployz deployment redeploy 1 --detach
# exit 0
```

stdout:
```
Deployment #2 of shop/production: queued
  redis: pending
  postgres: pending
  worker: pending
  api: pending
  web: pending
  pg-data: pending
  sentry: pending
```

### deployment redeploy, unknown

```console
$ ployz deployment redeploy 99
# exit 1
```

stderr:
```
error: shop/production has no Deployment #99
next: ployz deployment ls
```

## deploy

### deploy --plan

```console
$ ployz deploy --plan
# exit 0
```

stdout:
```
worker (update)
  worker.domains: added myworker.ada.ployz.test
api (update)
  api.image: "traefik/whoami:v1.10.3" → "traefik/whoami:v1.11.0"
  api.domains: added myapi.ada.ployz.test
  api.env.FEATURE_SEARCH: null → "on"
Couldn't check ingress: Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections.
Decided by the Servers when it runs: which containers start and stop.
```

stderr:
```
next: ployz deploy --expect-version 21:1:2.1 --project shop
```

### deploy --plan --json

```console
$ ployz deploy --plan --json
# exit 0
```

stdout:
```
{
  "changes": [
    {
      "comparison": "head",
      "data": null,
      "id": "99e8f125-0dd3-4a14-baa8-8244c36e8cd9",
      "lifecycle": "update",
      "name": "worker",
      "restarts": [],
      "row": "99e8f125-0dd3-4a14-baa8-8244c36e8cd9:node",
      "settings": [
        {
          "after": [
            {
              "prefix": "myworker",
              "targetPort": null
            }
          ],
          "before": [],
          "canRestore": false,
          "hostnames": {
            "myworker": "myworker.ada.ployz.test"
          },
          "kind": "update",
          "path": "worker.managedHostnames",
          "row": "99e8f125-0dd3-4a14-baa8-8244c36e8cd9:managedHostnames"
        }
      ],
      "type": "service"
    },
    {
      "comparison": "head",
      "data": null,
      "id": "d25db7f1-82e7-4e78-ad08-a12f91c227d2",
      "lifecycle": "update",
      "name": "api",
      "restarts": [],
      "row": "d25db7f1-82e7-4e78-ad08-a12f91c227d2:node",
      "settings": [
        {
          "after": {
            "credentials": false,
            "image": "traefik/whoami:v1.11.0",
            "type": "image"
          },
          "before": {
            "credentials": false,
            "image": "traefik/whoami:v1.10.3",
            "type": "image"
          },
          "canRestore": true,
          "kind": "update",
          "path": "api.source",
          "row": "d25db7f1-82e7-4e78-ad08-a12f91c227d2:source"
        },
        {
          "after": [
            {
              "prefix": "myapi",
              "targetPort": null
            }
          ],
          "before": [],
          "canRestore": false,
          "hostnames": {
            "myapi": "myapi.ada.ployz.test"
          },
          "kind": "update",
          "path": "api.managedHostnames",
          "row": "d25db7f1-82e7-4e78-ad08-a12f91c227d2:managedHostnames"
        },
        {
          "after": "on",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "api.env.FEATURE_SEARCH",
          "row": "d25db7f1-82e7-4e78-ad08-a12f91c227d2:variables.FEATURE_SEARCH"
        }
      ],
      "type": "service"
    }
  ],
  "domain_changes": [
    {
      "change": "added myworker.ada.ployz.test",
      "path": "worker.domains"
    },
    {
      "change": "added myapi.ada.ployz.test",
      "path": "api.domains"
    }
  ],
  "environment": {
    "id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "ingress_unchecked": [
    "Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections"
  ],
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 21:1:2.1 --project shop",
  "unresolved": [
    "operations"
  ],
  "version": "21:1:2.1"
}
```

### deploy --detach --message ...

```console
$ ployz deploy --detach --message Ship\ it
# exit 0
```

stdout:
```
Deployment #3 of shop/production: queued
  redis: pending
  postgres: pending
  worker: pending
  api: pending
  web: pending
  pg-data: pending
  sentry: pending
```

### deploy --detach --json

```console
$ ployz deploy --detach --json
# exit 0
```

stdout:
```
{
  "admitted_at": 1792494294,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": null,
  "environment": {
    "id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "environment_id": "41cccaac-61fd-42f4-8429-3eba71af5b34",
  "held": false,
  "id": "7b32e974-9565-4da9-af04-d6047a95791f",
  "in_flight": true,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 4 --project shop",
  "nodes": [
    {
      "id": "030ddc04-33d9-4149-a489-ee4788da4aa6",
      "name": "redis",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "54b62342-54f6-46c0-aa79-eaa89c03f4bd",
      "name": "postgres",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "99e8f125-0dd3-4a14-baa8-8244c36e8cd9",
      "name": "worker",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "d25db7f1-82e7-4e78-ad08-a12f91c227d2",
      "name": "api",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "f59287b3-06ba-4d82-aa22-f3ff607bc95f",
      "name": "web",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "313a7879-5239-4f45-8d0a-978eb58a8994",
      "name": "pg-data",
      "outcome": "pending",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "46b22ea1-47ad-4f35-baeb-4d044871c79c",
      "name": "sentry",
      "outcome": "pending",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 4,
  "outcome": null,
  "preview": null,
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": null,
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 2,
  "services": [],
  "ships": [
    "redis",
    "postgres",
    "worker",
    "api",
    "web"
  ],
  "started_at": null,
  "status": "queued",
  "upload": null
}
```

### deploy (follows; Cloud refuses the Cluster Domain)

```console
$ ployz deploy
# exit 124
```

### deploy unknown service

```console
$ ployz deploy nosuch --plan
# exit 1
```

stderr:
```
error: No Service named nosuch to deploy
```

### deploy --expect-version malformed

```console
$ ployz deploy --expect-version 1 --detach
# exit 2
```

stderr:
```
error: invalid value '1' for '--expect-version <VERSION>': Use the version ployz diff --json or a refusal gave, such as 3:1:0.1

For more information, try '--help'.
```

### deployment ls after deploys

```console
$ ployz deployment ls
# exit 0
```

stdout:
```
DEPLOYMENT	STATUS	SAVED REVISION
#5	queued	2
#4	superseded	2
#3	superseded	2
#2	superseded	1
#1	cancelled	1
```

## logs / ps / link / up

### ps

```console
$ ployz ps
# exit 1
```

stderr:
```
error: Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections
```

### ps --json

```console
$ ployz ps --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unavailable","details":{"refused":[{"machine_id":"0123456789abcdef0123456789abcdef","message":"invalid management connections"}],"unreachable":[]},"message":"Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections"}}
```

### logs

```console
$ ployz logs
# exit 1
```

stderr:
```
error: Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections
```

### logs web --json

```console
$ ployz logs web --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unavailable","details":{"refused":[{"machine_id":"0123456789abcdef0123456789abcdef","message":"invalid management connections"}],"unreachable":[]},"message":"Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections"}}
```

### logs --deployment 1 --build

```console
$ ployz logs --deployment 1 --build
# exit 0
```

stderr:
```
That Deployment built nothing.
```

### link --project shop (from a fresh directory)

```console
$ ployz link --project shop
# exit 0
```

stdout:
```
Linked /tmp/pz-cloud-capture/app to shop/production.
```

stderr:
```
next: ployz status
```

### status after link

```console
$ ployz status
# exit 0
```

stdout:
```
cloud = http://localhost:44553
organization = ada
signed in as = ada@example.com
environment = shop/production
linked from = /tmp/pz-cloud-capture/app
staged = 0 changes
deploying = #5 queued
```

stderr:
```
next: ployz deployment show 5
```

### up in an empty directory

```console
$ ployz up
# exit 2
```

stderr:
```
error: Nothing to deploy: /tmp/pz-cloud-capture/up-app is empty. Run ployz up in your app's directory.
```

### up in a directory with a Dockerfile --detach

```console
$ ployz up --detach
# exit 0
```

stdout:
```
Deployment #1 of up-app/production: queued
  uploaded by Ada Lovelace, sha256 d151b2ea8aab
  up-app: pending
Open https://up-app.ada.ployz.test
Dashboard: http://localhost:44553/cloud/ada/up-app/production
```

stderr:
```
Created Project up-app.
Added Service up-app.
```

### up --json --detach

```console
$ ployz up --json --detach
# exit 0
```

stdout:
```
{
  "dashboard": "http://localhost:44553/cloud/ada/up-app/production",
  "deployment": {
    "admitted_at": 1792494325,
    "admitted_by": "Ada Lovelace",
    "builds": [],
    "ci": null,
    "ended_at": null,
    "environment": {
      "id": "453ff61b-e090-4388-9e80-762ed7a44232",
      "name": "production",
      "project": "up-app",
      "revision": 4
    },
    "environment_id": "453ff61b-e090-4388-9e80-762ed7a44232",
    "held": false,
    "id": "8c69c1b0-d35f-4aa0-9329-5742964e59ba",
    "in_flight": true,
    "message": null,
    "namespace": "up-app-production",
    "nodes": [
      {
        "id": "dc19b62d-3e42-427f-bbf2-f1b410dee162",
        "name": "up-app",
        "outcome": "pending",
        "rows": [],
        "type": "service"
      }
    ],
    "number": 2,
    "outcome": null,
    "preview": null,
    "push": null,
    "remove": false,
    "rolls_back": [],
    "runner": null,
    "runtime_names": {
      "up-app": "up-app"
    },
    "saved": 1,
    "services": [],
    "ships": [
      "up-app"
    ],
    "started_at": null,
    "status": "queued",
    "upload": {
      "base": null,
      "digest": "d151b2ea8aabd095f7069ad872414940751f51b6139be8400047d2e6e293a3bc",
      "uploader": "Ada Lovelace"
    }
  },
  "directory": "/tmp/pz-cloud-capture/up-app",
  "next": "ployz deployment show 2",
  "urls": [
    "https://up-app.ada.ployz.test"
  ]
}
```

## cloud reset (no founding in progress)

### cloud reset (no -y)

```console
$ ployz cloud reset
# exit 2
```

stderr:
```
error: cloud reset gives up the unfinished founding; stop or erase the founding Server, then confirm with --yes
retry: ployz cloud reset --yes
```

### cloud reset -y

```console
$ ployz cloud reset -y
# exit 1
```

stderr:
```
error: The request conflicts with the current state.
```

### cloud reset -y --json

```console
$ ployz cloud reset -y --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"conflict","details":null,"message":"The request conflicts with the current state."}}
```

