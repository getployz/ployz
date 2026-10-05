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
#1 queued Saved revision 1 939f4f20-8e4a-40a5-a812-52c2c614d9f6
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
      "admitted_at": 1791162709,
      "admitted_by": "Ada Lovelace",
      "ended_at": null,
      "environment_id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
      "id": "939f4f20-8e4a-40a5-a812-52c2c614d9f6",
      "in_flight": true,
      "message": "Ship everything",
      "number": 1,
      "outcome": null,
      "remove": false,
      "runner": null,
      "saved": 1,
      "services": [],
      "started_at": null,
      "status": "queued",
      "upload": null
    }
  ],
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 17
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
  worker: pending
  postgres: pending
  pg-data: pending
```

### deployment show 1 --json

```console
$ ployz deployment show 1 --json
# exit 0
```

stdout:
```
{
  "admitted_at": 1791162709,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": null,
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 17
  },
  "environment_id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
  "id": "939f4f20-8e4a-40a5-a812-52c2c614d9f6",
  "in_flight": true,
  "message": "Ship everything",
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "1cb6967d-ebfb-4cee-b91a-b75b7f75b4ca",
      "name": "web",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "20379db8-85e1-4322-9a7b-d0463aa962a8",
      "name": "api",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "5a4d8764-d60a-4029-8e8c-c3ab8c91f3c6",
      "name": "worker",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "e2a9bbf3-849c-4173-98d6-274a7bf450df",
      "name": "postgres",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "49163a7c-9809-4325-8000-4d6a83f39bd6",
      "name": "pg-data",
      "outcome": "pending",
      "type": "volume"
    }
  ],
  "number": 1,
  "outcome": null,
  "preview": null,
  "remove": false,
  "runner": null,
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 1,
  "services": [],
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
shop/production has no Deployment #99
next: ployz deployment ls
```

### deployment show unknown number --json

```console
$ ployz deployment show 99 --json
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"next":"ployz deployment ls"},"message":"shop/production has no Deployment #99"}}
```

### deployment show garbage id

```console
$ ployz deployment show not-an-id
# exit 2
```

stderr:
```
Expected a Deployment ID or number
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
  worker: pending
  postgres: pending
  pg-data: pending
```

### deployment start 1 (follows; no Server)

```console
$ ployz deployment start 1
# exit 124
```

stdout:
```
Following Deployment #1; stopping this leaves it running.
queued: web pending, api pending, worker pending, postgres pending, pg-data pending
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
  worker: not attempted
  postgres: not attempted
  pg-data: not attempted
```

### deployment cancel 1 again --json

```console
$ ployz deployment cancel 1 --json
# exit 0
```

stdout:
```
{
  "admitted_at": 1791162709,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791162787,
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 17
  },
  "environment_id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
  "id": "939f4f20-8e4a-40a5-a812-52c2c614d9f6",
  "in_flight": false,
  "message": "Ship everything",
  "namespace": "shop-production",
  "next": "ployz deployment show 939f4f20-8e4a-40a5-a812-52c2c614d9f6",
  "nodes": [
    {
      "id": "1cb6967d-ebfb-4cee-b91a-b75b7f75b4ca",
      "name": "web",
      "outcome": "not_attempted",
      "type": "service"
    },
    {
      "id": "20379db8-85e1-4322-9a7b-d0463aa962a8",
      "name": "api",
      "outcome": "not_attempted",
      "type": "service"
    },
    {
      "id": "5a4d8764-d60a-4029-8e8c-c3ab8c91f3c6",
      "name": "worker",
      "outcome": "not_attempted",
      "type": "service"
    },
    {
      "id": "e2a9bbf3-849c-4173-98d6-274a7bf450df",
      "name": "postgres",
      "outcome": "not_attempted",
      "type": "service"
    },
    {
      "id": "49163a7c-9809-4325-8000-4d6a83f39bd6",
      "name": "pg-data",
      "outcome": "not_attempted",
      "type": "volume"
    }
  ],
  "number": 1,
  "outcome": null,
  "preview": null,
  "remove": false,
  "runner": null,
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 1,
  "services": [],
  "started_at": null,
  "status": "cancelled",
  "upload": null
}
```

### deployment retry 1 --detach

```console
$ ployz deployment retry 1 --detach
# exit 0
```

stdout:
```
Deployment #2 of shop/production: queued
  web: pending
  api: pending
  worker: pending
  postgres: pending
  pg-data: pending
```

### deployment retry, unknown

```console
$ ployz deployment retry 99
# exit 1
```

stderr:
```
shop/production has no Deployment #99
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
  api.source: {"credentials":false,"image":"ghcr.io/acme/api:1.4","type":"image"} -> {"credentials":false,"image":"ghcr.io/acme/api:1.5","type":"image"}
  api.managedHostnames: [] -> [{"prefix":"myapi","targetPort":null}]
  api.env.FEATURE_SEARCH: null -> "on"
worker (update)
  worker.managedHostnames: [] -> [{"prefix":"myworker","targetPort":null}]
Decided by the Servers when it runs: operations.
next: ployz deploy --expect-version 17:1:2.1
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
      "id": "20379db8-85e1-4322-9a7b-d0463aa962a8",
      "lifecycle": "update",
      "name": "api",
      "row": "20379db8-85e1-4322-9a7b-d0463aa962a8:node",
      "settings": [
        {
          "after": {
            "credentials": false,
            "image": "ghcr.io/acme/api:1.5",
            "type": "image"
          },
          "before": {
            "credentials": false,
            "image": "ghcr.io/acme/api:1.4",
            "type": "image"
          },
          "canRestore": true,
          "kind": "update",
          "path": "api.source",
          "row": "20379db8-85e1-4322-9a7b-d0463aa962a8:source"
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
          "kind": "update",
          "path": "api.managedHostnames",
          "row": "20379db8-85e1-4322-9a7b-d0463aa962a8:managedHostnames"
        },
        {
          "after": "on",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "api.env.FEATURE_SEARCH",
          "row": "20379db8-85e1-4322-9a7b-d0463aa962a8:variables.FEATURE_SEARCH"
        }
      ],
      "type": "service"
    },
    {
      "comparison": "head",
      "data": null,
      "id": "5a4d8764-d60a-4029-8e8c-c3ab8c91f3c6",
      "lifecycle": "update",
      "name": "worker",
      "row": "5a4d8764-d60a-4029-8e8c-c3ab8c91f3c6:node",
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
          "kind": "update",
          "path": "worker.managedHostnames",
          "row": "5a4d8764-d60a-4029-8e8c-c3ab8c91f3c6:managedHostnames"
        }
      ],
      "type": "service"
    }
  ],
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 17
  },
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 17:1:2.1",
  "unresolved": [
    "operations"
  ],
  "version": "17:1:2.1"
}
```

### deploy --detach --message ...

```console
$ ployz deploy --detach --message Ship\ it
# exit 1
```

stderr:
```
Cloud couldn't reserve the Cluster Domain; deploy again.
```

### deploy --detach --json

```console
$ ployz deploy --detach --json
# exit 1
```

stdout:
```
{"error":{"code":"unavailable","details":null,"message":"Cloud couldn't reserve the Cluster Domain; deploy again."}}
```

### deploy (follows; Cloud refuses the Cluster Domain)

```console
$ ployz deploy
# exit 1
```

stderr:
```
Cloud couldn't reserve the Cluster Domain; deploy again.
```

### deploy unknown service

```console
$ ployz deploy nosuch --plan
# exit 1
```

stderr:
```
No Service named nosuch to deploy
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
#2 queued Saved revision 1 2cf9b89b-bf9c-45cb-a67f-8169844dcbea
#1 cancelled Saved revision 1 939f4f20-8e4a-40a5-a812-52c2c614d9f6
```

## logs / ps / link / up

### ps

```console
$ ployz ps
# exit 1
```

stderr:
```
no Server of this Organization is reachable now: 0123456789abcdef0123456789abcdef
```

### ps --json

```console
$ ployz ps --json
# exit 1
```

stdout:
```
{"error":{"code":"unavailable","details":{"unreachable":["0123456789abcdef0123456789abcdef"]},"message":"no Server of this Organization is reachable now: 0123456789abcdef0123456789abcdef"}}
```

### logs

```console
$ ployz logs
# exit 1
```

stderr:
```
no Server of this Organization is reachable now: 0123456789abcdef0123456789abcdef
```

### logs web --json

```console
$ ployz logs web --json
# exit 1
```

stdout:
```
{"error":{"code":"unavailable","details":{"unreachable":["0123456789abcdef0123456789abcdef"]},"message":"no Server of this Organization is reachable now: 0123456789abcdef0123456789abcdef"}}
```

### logs --deployment 1 --build

```console
$ ployz logs --deployment 1 --build
# exit 0
```

stdout:
```
Deployment 939f4f20-8e4a-40a5-a812-52c2c614d9f6 built nothing.
```

### link (from a fresh directory)

```console
$ ployz link
# exit 0
```

stdout:
```
Linked /tmp/pz-cloud-capture/app to shop/production.
```

### status after link

```console
$ ployz status
# exit 0
```

stdout:
```
Cloud: http://localhost:34733
Organization ada (ada@example.com).
Environment shop/production.
Linked from /tmp/pz-cloud-capture/app.
4 staged changes.
Deployment 2 is queued.
next: ployz diff
```

### up in an empty directory

```console
$ ployz up
# exit 1
```

stdout:
```
Created Project up-app.
Added Service up-app.
```

stderr:
```
Cloud couldn't reserve the Cluster Domain; deploy again.
```

### up in a directory with a Dockerfile --detach

```console
$ ployz up --detach
# exit 1
```

stderr:
```
Cloud couldn't reserve the Cluster Domain; deploy again.
```

### up --json --detach

```console
$ ployz up --json --detach
# exit 1
```

stdout:
```
{"error":{"code":"unavailable","details":null,"message":"Cloud couldn't reserve the Cluster Domain; deploy again."}}
```

## cloud reset (no founding in progress)

### cloud reset (no -y)

```console
$ ployz cloud reset
# exit 1
```

stderr:
```
cloud reset gives up the unfinished founding; stop or erase the founding Server, then confirm with --yes
next: ployz cloud reset --yes
```

### cloud reset -y

```console
$ ployz cloud reset -y
# exit 1
```

stderr:
```
Cloud answered HTTP 409: {"_tag":"PublicError","code":"CONFLICT","message":"The request conflicts with the current state."}
```

### cloud reset -y --json

```console
$ ployz cloud reset -y --json
# exit 1
```

stdout:
```
{"error":{"code":"conflict","details":null,"message":"Cloud answered HTTP 409: {\"_tag\":\"PublicError\",\"code\":\"CONFLICT\",\"message\":\"The request conflicts with the current state.\"}"}}
```

