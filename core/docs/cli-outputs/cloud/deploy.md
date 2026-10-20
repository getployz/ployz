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
      "admitted_at": 1792489758,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": null,
      "environment_id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
      "held": false,
      "id": "550eafae-bd03-4720-ba7c-9271a8f2bbfe",
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
        "web",
        "api",
        "redis",
        "worker",
        "postgres"
      ],
      "started_at": null,
      "status": "queued",
      "upload": null
    }
  ],
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "in_flight": [
    {
      "admitted_at": 1792489758,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": null,
      "environment_id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
      "held": false,
      "id": "550eafae-bd03-4720-ba7c-9271a8f2bbfe",
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
        "web",
        "api",
        "redis",
        "worker",
        "postgres"
      ],
      "started_at": null,
      "status": "queued",
      "upload": null
    }
  ],
  "latest": {
    "admitted_at": 1792489758,
    "admitted_by": "Ada Lovelace",
    "ci": null,
    "ended_at": null,
    "environment_id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "held": false,
    "id": "550eafae-bd03-4720-ba7c-9271a8f2bbfe",
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
      "web",
      "api",
      "redis",
      "worker",
      "postgres"
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
  web: pending
  api: pending
  redis: pending
  worker: pending
  postgres: pending
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
  "admitted_at": 1792489758,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": null,
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "environment_id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
  "held": false,
  "id": "550eafae-bd03-4720-ba7c-9271a8f2bbfe",
  "in_flight": true,
  "message": "Ship everything",
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "1c4d35e0-f081-4c62-af72-2e7103a71678",
      "name": "web",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "5698b0ec-d906-43b0-8c40-532d58e780ba",
      "name": "api",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "5da25d66-034b-4049-ae6c-c7631bc12e19",
      "name": "redis",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "bf6d0b48-8b62-483f-900c-ee4c97b6e9a9",
      "name": "worker",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0ce950e-88e1-4ec1-a11b-fa2337519b6f",
      "name": "postgres",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "b115abc4-23f5-40cd-8bc4-483ed2bd65bb",
      "name": "pg-data",
      "outcome": "pending",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "4849f9d4-0fae-4e37-968a-cfddb92da2e3",
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
    "web",
    "api",
    "redis",
    "worker",
    "postgres"
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
  web: pending
  api: pending
  redis: pending
  worker: pending
  postgres: pending
  pg-data: pending
  sentry: pending
```

### deployment start 1 (follows; no Server)

```console
$ ployz deployment start 1
# exit 124
```

### deployment cancel 1

```console
$ ployz deployment cancel 1
# exit 0
```

stdout:
```
Deployment #1 of shop/production: cancelled
  web: not attempted
  api: not attempted
  redis: not attempted
  worker: not attempted
  postgres: not attempted
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
  "admitted_at": 1792489758,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792489972,
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "environment_id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
  "held": false,
  "id": "550eafae-bd03-4720-ba7c-9271a8f2bbfe",
  "in_flight": false,
  "message": "Ship everything",
  "namespace": "shop-production",
  "next": "ployz deployment show 1 --project shop",
  "nodes": [
    {
      "id": "1c4d35e0-f081-4c62-af72-2e7103a71678",
      "name": "web",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "5698b0ec-d906-43b0-8c40-532d58e780ba",
      "name": "api",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "5da25d66-034b-4049-ae6c-c7631bc12e19",
      "name": "redis",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "bf6d0b48-8b62-483f-900c-ee4c97b6e9a9",
      "name": "worker",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0ce950e-88e1-4ec1-a11b-fa2337519b6f",
      "name": "postgres",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "b115abc4-23f5-40cd-8bc4-483ed2bd65bb",
      "name": "pg-data",
      "outcome": "not_attempted",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "4849f9d4-0fae-4e37-968a-cfddb92da2e3",
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
    "web",
    "api",
    "redis",
    "worker",
    "postgres"
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
  web: pending
  api: pending
  redis: pending
  worker: pending
  postgres: pending
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
api (update)
  api.image: "traefik/whoami:v1.10.3" → "traefik/whoami:v1.11.0"
  api.domains: added myapi.ada.ployz.test
  api.env.FEATURE_SEARCH: null → "on"
worker (update)
  worker.domains: added myworker.ada.ployz.test
Couldn't check ingress: Server 0123456789abcdef0123456789abcdef couldn't give this command access: invalid management connections.
Decided by the Servers when it runs: operations.
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
      "id": "5698b0ec-d906-43b0-8c40-532d58e780ba",
      "lifecycle": "update",
      "name": "api",
      "restarts": [],
      "row": "5698b0ec-d906-43b0-8c40-532d58e780ba:node",
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
          "row": "5698b0ec-d906-43b0-8c40-532d58e780ba:source"
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
          "row": "5698b0ec-d906-43b0-8c40-532d58e780ba:managedHostnames"
        },
        {
          "after": "on",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "api.env.FEATURE_SEARCH",
          "row": "5698b0ec-d906-43b0-8c40-532d58e780ba:variables.FEATURE_SEARCH"
        }
      ],
      "type": "service"
    },
    {
      "comparison": "head",
      "data": null,
      "id": "bf6d0b48-8b62-483f-900c-ee4c97b6e9a9",
      "lifecycle": "update",
      "name": "worker",
      "restarts": [],
      "row": "bf6d0b48-8b62-483f-900c-ee4c97b6e9a9:node",
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
          "row": "bf6d0b48-8b62-483f-900c-ee4c97b6e9a9:managedHostnames"
        }
      ],
      "type": "service"
    }
  ],
  "domain_changes": [
    {
      "change": "added myapi.ada.ployz.test",
      "path": "api.domains"
    },
    {
      "change": "added myworker.ada.ployz.test",
      "path": "worker.domains"
    }
  ],
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
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
  web: pending
  api: pending
  redis: pending
  worker: pending
  postgres: pending
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
  "admitted_at": 1792489981,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": null,
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "environment_id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
  "held": false,
  "id": "cd77eeaa-717e-4a5a-9572-5673ed049251",
  "in_flight": true,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 4 --project shop",
  "nodes": [
    {
      "id": "1c4d35e0-f081-4c62-af72-2e7103a71678",
      "name": "web",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "5698b0ec-d906-43b0-8c40-532d58e780ba",
      "name": "api",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "5da25d66-034b-4049-ae6c-c7631bc12e19",
      "name": "redis",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "bf6d0b48-8b62-483f-900c-ee4c97b6e9a9",
      "name": "worker",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0ce950e-88e1-4ec1-a11b-fa2337519b6f",
      "name": "postgres",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "b115abc4-23f5-40cd-8bc4-483ed2bd65bb",
      "name": "pg-data",
      "outcome": "pending",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "4849f9d4-0fae-4e37-968a-cfddb92da2e3",
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
    "web",
    "api",
    "redis",
    "worker",
    "postgres"
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
cloud = http://localhost:35249
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
Dashboard: http://localhost:35249/cloud/ada/up-app/production
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
  "dashboard": "http://localhost:35249/cloud/ada/up-app/production",
  "deployment": {
    "admitted_at": 1792490009,
    "admitted_by": "Ada Lovelace",
    "builds": [],
    "ci": null,
    "ended_at": null,
    "environment": {
      "id": "e3c4d120-7c04-422e-ae1a-7361dc675089",
      "name": "production",
      "project": "up-app",
      "revision": 4
    },
    "environment_id": "e3c4d120-7c04-422e-ae1a-7361dc675089",
    "held": false,
    "id": "8150da84-5f1f-4257-b013-fe8953dfac00",
    "in_flight": true,
    "message": null,
    "namespace": "up-app-production",
    "nodes": [
      {
        "id": "6f04619b-66df-4741-a35f-95bcc57c81d9",
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

