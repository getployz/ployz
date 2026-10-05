# Deploy and Deployments through Cloud (real Server)

## Nothing staged

### deploy --plan, nothing staged

```console
$ ployz deploy --plan
# exit 0
```

stdout:
```
No authored changes to deploy in shop/production.
Decided by the Servers when it runs: operations.
next: ployz deploy --expect-version 26:16:0.32
```

### deploy --plan --json, nothing staged

```console
$ ployz --json deploy --plan
# exit 0
```

stdout:
```
{
  "changes": [],
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 26
  },
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 26:16:0.32",
  "unresolved": [
    "operations"
  ],
  "version": "26:16:0.32"
}
```

### deploy, nothing staged

```console
$ ployz deploy
# exit 0
```

stdout:
```
Following Deployment #36; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
applied: web unchanged, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #36 of shop/production: applied
  web: unchanged
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### deploy --json, nothing staged

```console
$ ployz --json deploy
# exit 0
```

stdout:
```
{
  "admitted_at": 1791166894,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791166896,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 26
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "fc349b03-bc10-4fb1-8e13-8151a59c8bca",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show fc349b03-bc10-4fb1-8e13-8151a59c8bca",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "unchanged",
      "type": "volume"
    }
  ],
  "number": 37,
  "outcome": {
    "reason": null,
    "summary": {
      "completed": 0,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [],
    "would_remove": []
  },
  "remove": false,
  "runner": "cloud-01M44XVMVT9ZAT3E0EVT6CDE4F",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 16,
  "services": [],
  "started_at": 1791166895,
  "status": "applied",
  "upload": null
}
```

stderr:
```
Following Deployment #37; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
applied: web unchanged, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
```

## Staged change

### stage a change

```console
$ ployz set web.env.GREETING=hello
# exit 0
```

stdout:
```
Staged web.env.GREETING in shop/production (revision 27).
```

### diff

```console
$ ployz diff
# exit 0
```

stdout:
```
web (update)
  web.env.GREETING: null -> "hello"
next: ployz deploy --expect-version 27:16:0.34
```

### deploy --plan with a staged change

```console
$ ployz deploy --plan
# exit 0
```

stdout:
```
web (update)
  web.env.GREETING: null -> "hello"
Decided by the Servers when it runs: operations.
next: ployz deploy --expect-version 27:16:0.34
```

### deploy --plan --json with a staged change

```console
$ ployz --json deploy --plan
# exit 0
```

stdout:
```
{
  "changes": [
    {
      "comparison": "head",
      "data": null,
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "lifecycle": "update",
      "name": "web",
      "row": "8cb17782-fd07-45b5-9009-992bb7f6f24d:node",
      "settings": [
        {
          "after": "hello",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "web.env.GREETING",
          "row": "8cb17782-fd07-45b5-9009-992bb7f6f24d:variables.GREETING"
        }
      ],
      "type": "service"
    }
  ],
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 27
  },
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 27:16:0.34",
  "unresolved": [
    "operations"
  ],
  "version": "27:16:0.34"
}
```

### deploy a staged change (live follower) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Following Deployment #38; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
applied: web deployed, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #38 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
# exit 0

```

### deploy a staged change (pipe)

```console
$ ployz deploy --message Say\ hi
# exit 0
```

stdout:
```
Following Deployment #39; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
applied: web deployed, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #39 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### deploy --json a staged change

```console
$ ployz --json deploy
# exit 0
```

stdout:
```
{
  "admitted_at": 1791166917,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791166925,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 29
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "24dbeed8-8db1-410a-96c7-4f54da96189e",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 24dbeed8-8db1-410a-96c7-4f54da96189e",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "deployed",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "unchanged",
      "type": "volume"
    }
  ],
  "number": 40,
  "outcome": {
    "reason": null,
    "summary": {
      "completed": 1,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "web-8694",
        "index": 0,
        "machine_id": "7784f1a17df144728819cecf47282904",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "7784f1a17df144728819cecf47282904",
          "old_container_id": "30aa6866a0d92f2dcb0d5426cb18b9bdc4bcdaff66e54276f15ede8993df8647",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "nginx:1.27-alpine",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "8cb17782-fd07-45b5-9009-992bb7f6f24d"
              },
              "log_driver": null,
              "open_stdin": false,
              "pid_mode": null,
              "privileged": false,
              "pull_policy": "missing",
              "resources": {
                "cpu_nanos": null,
                "device_reservations": [],
                "devices": [],
                "memory_bytes": null,
                "memory_reservation_bytes": null,
                "shared_memory_bytes": null,
                "ulimits": {}
              },
              "restart": {
                "name": "unless-stopped"
              },
              "stop_timeout_secs": null,
              "sysctls": {},
              "tty": false,
              "user": null,
              "working_directory": null
            },
            "mode": {
              "mode": "replicated",
              "replicas": 1
            },
            "mounts": [],
            "name": "web",
            "placement": {
              "constraints": []
            },
            "ports": [
              {
                "container_port": 8080,
                "hostname": "acme.com",
                "http_protocol": "https",
                "load_balancer_port": 443,
                "mode": "ingress"
              },
              {
                "container_port": 80,
                "hostname": "web.ada.ployz.test",
                "http_protocol": "https",
                "load_balancer_port": 443,
                "mode": "ingress"
              }
            ],
            "pre_deploy": null,
            "service_id": "6d5c9a3fb9d647d9a81bfe273e48dde6",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "replace_container"
        },
        "service_name": "web",
        "status": {
          "type": "pending"
        }
      }
    ],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [
      {
        "message": "acme.com answers from another server. Point it at 203.0.113.10. A certificate cannot be issued until then.",
        "type": "ingress_hostname"
      },
      {
        "message": "web.ada.ployz.test does not resolve. Add a DNS record pointing at 203.0.113.10. A certificate cannot be issued until then.",
        "type": "ingress_hostname"
      }
    ],
    "would_remove": []
  },
  "remove": false,
  "runner": "cloud-01M44XWAN3M32PWNNNSQJ2AWYZ",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 19,
  "services": [],
  "started_at": 1791166917,
  "status": "applied",
  "upload": null
}
```

stderr:
```
Following Deployment #40; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
applied: web deployed, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
```

## Detach

### deploy --detach

```console
$ ployz deploy --detach
# exit 0
```

stdout:
```
Deployment #41 of shop/production: queued
  web: pending
  api: pending
  postgres: pending
  worker: pending
  pg-data: pending
```

### deploy --detach --json

```console
$ ployz --json deploy --detach
# exit 0
```

stdout:
```
{
  "admitted_at": 1791166936,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": null,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 31
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "e400c76f-1d28-443f-a5e9-ae4338441650",
  "in_flight": true,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show e400c76f-1d28-443f-a5e9-ae4338441650",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "pending",
      "type": "volume"
    }
  ],
  "number": 42,
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
  "saved": 21,
  "services": [],
  "started_at": null,
  "status": "queued",
  "upload": null
}
```

## Deployment ls and show

### deployment ls

```console
$ ployz deployment ls --limit 5
# exit 0
```

stdout:
```
#42 applied Saved revision 21 e400c76f-1d28-443f-a5e9-ae4338441650
#41 applied Saved revision 20 bb9f7265-3fb0-4c65-9783-3a8884c30ad5
#40 applied Saved revision 19 24dbeed8-8db1-410a-96c7-4f54da96189e
#39 applied Saved revision 18 620449c2-5985-4654-a6df-d9e13cd36102
#38 applied Saved revision 17 23b4fc49-3059-4b60-951f-dabad70a4cc4
next: ployz deployment ls --cursor 38 --limit 5
```

### deployment ls --json

```console
$ ployz --json deployment ls --limit 2
# exit 0
```

stdout:
```
{
  "deployments": [
    {
      "admitted_at": 1791166936,
      "admitted_by": "Ada Lovelace",
      "ended_at": 1791166944,
      "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
      "id": "e400c76f-1d28-443f-a5e9-ae4338441650",
      "in_flight": false,
      "message": null,
      "number": 42,
      "outcome": {
        "reason": null,
        "summary": {
          "completed": 1,
          "type": "success"
        },
        "type": "executed"
      },
      "remove": false,
      "runner": "cloud-01M44XWXGC5TDPSXQYJYY7Y1SA",
      "saved": 21,
      "services": [],
      "started_at": 1791166936,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1791166927,
      "admitted_by": "Ada Lovelace",
      "ended_at": 1791166934,
      "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
      "id": "bb9f7265-3fb0-4c65-9783-3a8884c30ad5",
      "in_flight": false,
      "message": null,
      "number": 41,
      "outcome": {
        "reason": null,
        "summary": {
          "completed": 1,
          "type": "success"
        },
        "type": "executed"
      },
      "remove": false,
      "runner": "cloud-01M44XWMEHYH4Y0DASQ127NJ0V",
      "saved": 20,
      "services": [],
      "started_at": 1791166927,
      "status": "applied",
      "upload": null
    }
  ],
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 31
  },
  "next": "ployz deployment ls --cursor 41 --limit 2",
  "next_cursor": "41"
}
```

### deployment show by id

```console
$ ployz deployment show e400c76f-1d28-443f-a5e9-ae4338441650
# exit 0
```

stdout:
```
Deployment #42 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### deployment show by number

```console
$ ployz deployment show 42
# exit 0
```

stdout:
```
Deployment #42 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### deployment show --json

```console
$ ployz --json deployment show e400c76f-1d28-443f-a5e9-ae4338441650
# exit 0
```

stdout:
```
{
  "admitted_at": 1791166936,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791166944,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 31
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "e400c76f-1d28-443f-a5e9-ae4338441650",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "deployed",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "unchanged",
      "type": "volume"
    }
  ],
  "number": 42,
  "outcome": {
    "reason": null,
    "summary": {
      "completed": 1,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "web-db73",
        "index": 0,
        "machine_id": "7784f1a17df144728819cecf47282904",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "7784f1a17df144728819cecf47282904",
          "old_container_id": "7c142aebd841e677489175ffa0c1a34099e5b8309315416ff9e2b771fbe09e9f",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "nginx:1.27-alpine",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "8cb17782-fd07-45b5-9009-992bb7f6f24d"
              },
              "log_driver": null,
              "open_stdin": false,
              "pid_mode": null,
              "privileged": false,
              "pull_policy": "missing",
              "resources": {
                "cpu_nanos": null,
                "device_reservations": [],
                "devices": [],
                "memory_bytes": null,
                "memory_reservation_bytes": null,
                "shared_memory_bytes": null,
                "ulimits": {}
              },
              "restart": {
                "name": "unless-stopped"
              },
              "stop_timeout_secs": null,
              "sysctls": {},
              "tty": false,
              "user": null,
              "working_directory": null
            },
            "mode": {
              "mode": "replicated",
              "replicas": 1
            },
            "mounts": [],
            "name": "web",
            "placement": {
              "constraints": []
            },
            "ports": [
              {
                "container_port": 8080,
                "hostname": "acme.com",
                "http_protocol": "https",
                "load_balancer_port": 443,
                "mode": "ingress"
              },
              {
                "container_port": 80,
                "hostname": "web.ada.ployz.test",
                "http_protocol": "https",
                "load_balancer_port": 443,
                "mode": "ingress"
              }
            ],
            "pre_deploy": null,
            "service_id": "6d5c9a3fb9d647d9a81bfe273e48dde6",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "replace_container"
        },
        "service_name": "web",
        "status": {
          "type": "pending"
        }
      }
    ],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [
      {
        "message": "acme.com answers from another server. Point it at 203.0.113.10. A certificate cannot be issued until then.",
        "type": "ingress_hostname"
      },
      {
        "message": "web.ada.ployz.test does not resolve. Add a DNS record pointing at 203.0.113.10. A certificate cannot be issued until then.",
        "type": "ingress_hostname"
      }
    ],
    "would_remove": []
  },
  "remove": false,
  "runner": "cloud-01M44XWXGC5TDPSXQYJYY7Y1SA",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 21,
  "services": [],
  "started_at": 1791166936,
  "status": "applied",
  "upload": null
}
```

## A failing Deployment (bad image)

### stage a bad image

```console
$ ployz set worker.image=ghcr.io/getployz/does-not-exist:nope
# exit 0
```

stdout:
```
Staged worker.image in shop/production (revision 32).
```

### deploy that fails

```console
$ ployz deploy
# exit 3
```

stdout:
```
Following Deployment #43; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web unchanged, api unchanged, postgres unchanged, worker pending, pg-data unchanged
failed: web unchanged, api unchanged, postgres unchanged, worker failed, pg-data unchanged
Deployment #43 of shop/production: failed
  worker: create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied
denied
  web: unchanged
  api: unchanged
  postgres: unchanged
  worker: failed
  pg-data: unchanged
```

### deploy that fails (live follower) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Following Deployment #44; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web unchanged, api unchanged, postgres unchanged, worker pending, pg-data unchanged
failed: web unchanged, api unchanged, postgres unchanged, worker failed, pg-data unchanged
Deployment #44 of shop/production: failed
  worker: create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied
denied
  web: unchanged
  api: unchanged
  postgres: unchanged
  worker: failed
  pg-data: unchanged
# exit 3

```

### deploy --json that fails

```console
$ ployz --json deploy
# exit 3
```

stdout:
```
{
  "admitted_at": 1791166953,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791166955,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 32
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "c405d65a-c1f7-4ea9-9fa4-d1286e12dfc9",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show c405d65a-c1f7-4ea9-9fa4-d1286e12dfc9",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "failed",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "unchanged",
      "type": "volume"
    }
  ],
  "number": 45,
  "outcome": {
    "reason": "worker: create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied\ndenied",
    "summary": {
      "completed": 0,
      "reason": "machine",
      "type": "failed",
      "unexecuted": 0
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "worker-949b",
        "index": 0,
        "machine_id": "7784f1a17df144728819cecf47282904",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "7784f1a17df144728819cecf47282904",
          "old_container_id": "5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e"
              },
              "log_driver": null,
              "open_stdin": false,
              "pid_mode": null,
              "privileged": false,
              "pull_policy": "missing",
              "resources": {
                "cpu_nanos": null,
                "device_reservations": [],
                "devices": [],
                "memory_bytes": null,
                "memory_reservation_bytes": null,
                "shared_memory_bytes": null,
                "ulimits": {}
              },
              "restart": {
                "name": "unless-stopped"
              },
              "stop_timeout_secs": null,
              "sysctls": {},
              "tty": false,
              "user": null,
              "working_directory": null
            },
            "mode": {
              "mode": "replicated",
              "replicas": 1
            },
            "mounts": [],
            "name": "worker",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "ec02437ef76a4d11a5af850934be313e",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "replace_container"
        },
        "service_name": "worker",
        "status": {
          "type": "pending"
        }
      }
    ],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [],
    "would_remove": []
  },
  "remove": false,
  "runner": "cloud-01M44XXED0JG224VBS7Q90C50E",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 22,
  "services": [],
  "started_at": 1791166953,
  "status": "failed",
  "upload": null
}
```

stderr:
```
Following Deployment #45; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
failed: web unchanged, api unchanged, postgres unchanged, worker failed, pg-data unchanged
```

### deployment show of the failed Deployment

```console
$ ployz deployment show c405d65a-c1f7-4ea9-9fa4-d1286e12dfc9
# exit 0
```

stdout:
```
Deployment #45 of shop/production: failed
  worker: create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied
denied
  web: unchanged
  api: unchanged
  postgres: unchanged
  worker: failed
  pg-data: unchanged
```

### deployment show --json of the failed Deployment

```console
$ ployz --json deployment show c405d65a-c1f7-4ea9-9fa4-d1286e12dfc9
# exit 0
```

stdout:
```
{
  "admitted_at": 1791166953,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791166955,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 32
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "c405d65a-c1f7-4ea9-9fa4-d1286e12dfc9",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "failed",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "unchanged",
      "type": "volume"
    }
  ],
  "number": 45,
  "outcome": {
    "reason": "worker: create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied\ndenied",
    "summary": {
      "completed": 0,
      "reason": "machine",
      "type": "failed",
      "unexecuted": 0
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "worker-949b",
        "index": 0,
        "machine_id": "7784f1a17df144728819cecf47282904",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "7784f1a17df144728819cecf47282904",
          "old_container_id": "5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e"
              },
              "log_driver": null,
              "open_stdin": false,
              "pid_mode": null,
              "privileged": false,
              "pull_policy": "missing",
              "resources": {
                "cpu_nanos": null,
                "device_reservations": [],
                "devices": [],
                "memory_bytes": null,
                "memory_reservation_bytes": null,
                "shared_memory_bytes": null,
                "ulimits": {}
              },
              "restart": {
                "name": "unless-stopped"
              },
              "stop_timeout_secs": null,
              "sysctls": {},
              "tty": false,
              "user": null,
              "working_directory": null
            },
            "mode": {
              "mode": "replicated",
              "replicas": 1
            },
            "mounts": [],
            "name": "worker",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "ec02437ef76a4d11a5af850934be313e",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "replace_container"
        },
        "service_name": "worker",
        "status": {
          "type": "pending"
        }
      }
    ],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [],
    "would_remove": []
  },
  "remove": false,
  "runner": "cloud-01M44XXED0JG224VBS7Q90C50E",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 22,
  "services": [],
  "started_at": 1791166953,
  "status": "failed",
  "upload": null
}
```

### deployment retry of the failed Deployment

```console
$ ployz deployment retry c405d65a-c1f7-4ea9-9fa4-d1286e12dfc9
# exit 3
```

stdout:
```
Following Deployment #46; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
failed: web unchanged, api unchanged, postgres unchanged, worker failed, pg-data unchanged
Deployment #46 of shop/production: failed
  worker: create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied
denied
  web: unchanged
  api: unchanged
  postgres: unchanged
  worker: failed
  pg-data: unchanged
```

### deployment retry --json of the failed Deployment

```console
$ ployz --json deployment retry 6398dce0-c9bc-4132-a919-65c236cc65fd
# exit 3
```

stdout:
```
{
  "admitted_at": 1791166959,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791166960,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 32
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "1b9cfb65-333b-41eb-b44d-1ce5c919f5cd",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 1b9cfb65-333b-41eb-b44d-1ce5c919f5cd",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "failed",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "unchanged",
      "type": "volume"
    }
  ],
  "number": 47,
  "outcome": {
    "reason": "worker: create Container failed: Docker operation failed: Docker responded with status code 500: error from registry: denied\ndenied",
    "summary": {
      "completed": 0,
      "reason": "machine",
      "type": "failed",
      "unexecuted": 0
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "worker-949b",
        "index": 0,
        "machine_id": "7784f1a17df144728819cecf47282904",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "7784f1a17df144728819cecf47282904",
          "old_container_id": "5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e"
              },
              "log_driver": null,
              "open_stdin": false,
              "pid_mode": null,
              "privileged": false,
              "pull_policy": "missing",
              "resources": {
                "cpu_nanos": null,
                "device_reservations": [],
                "devices": [],
                "memory_bytes": null,
                "memory_reservation_bytes": null,
                "shared_memory_bytes": null,
                "ulimits": {}
              },
              "restart": {
                "name": "unless-stopped"
              },
              "stop_timeout_secs": null,
              "sysctls": {},
              "tty": false,
              "user": null,
              "working_directory": null
            },
            "mode": {
              "mode": "replicated",
              "replicas": 1
            },
            "mounts": [],
            "name": "worker",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "ec02437ef76a4d11a5af850934be313e",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "replace_container"
        },
        "service_name": "worker",
        "status": {
          "type": "pending"
        }
      }
    ],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [],
    "would_remove": []
  },
  "remove": false,
  "runner": "cloud-01M44XXM19SD6YS69PVGTBNHQB",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 22,
  "services": [],
  "started_at": 1791166959,
  "status": "failed",
  "upload": null
}
```

stderr:
```
Following Deployment #47; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
failed: web unchanged, api unchanged, postgres unchanged, worker failed, pg-data unchanged
```

### fix the image

```console
$ ployz set worker.image=traefik/whoami:v1.10.3
# exit 0
```

stdout:
```
Staged worker.image in shop/production (revision 33).
```

### deploy the fix

```console
$ ployz deploy
# exit 0
```

stdout:
```
Following Deployment #48; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
applied: web unchanged, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #48 of shop/production: applied
  web: unchanged
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

## Queued Deployments: cancel and start

### deployment ls with queued Deployments

```console
$ ployz deployment ls --limit 4
# exit 0
```

stdout:
```
#51 queued Saved revision 25 a2533d8b-825c-44bf-afdd-50bcd714a426
#50 superseded Saved revision 25 b72485ba-95a0-429d-8eb8-1780fa6d6b5c
#49 running Saved revision 24 6a57912d-e0da-4603-a9cb-7dc872197099
#48 applied Saved revision 23 cc3a6d7a-f272-4a47-8ad4-d7939cbc9793
next: ployz deployment ls --cursor 48 --limit 4
```

### deployment cancel a queued Deployment

```console
$ ployz deployment cancel a2533d8b-825c-44bf-afdd-50bcd714a426
# exit 0
```

stdout:
```
Deployment #51 of shop/production: cancelled
  web: not attempted
  api: not attempted
  postgres: not attempted
  worker: not attempted
  pg-data: not attempted
```

### deployment cancel --json a queued Deployment

```console
$ ployz --json deployment cancel 6d26daad-ff8a-426f-8c39-fd991a00da7c
# exit 0
```

stdout:
```
{
  "admitted_at": 1791166976,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": null,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 35
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "6d26daad-ff8a-426f-8c39-fd991a00da7c",
  "in_flight": true,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 6d26daad-ff8a-426f-8c39-fd991a00da7c",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "pending",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "pending",
      "type": "volume"
    }
  ],
  "number": 53,
  "outcome": null,
  "preview": null,
  "remove": false,
  "runner": "cloud-01M44XY4EANZCEQZXK9HSQYS4C",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 25,
  "services": [],
  "started_at": 1791166976,
  "status": "cancelling",
  "upload": null
}
```

### deployment start a queued Deployment

```console
$ ployz deployment start b65dace4-36ba-4049-8f11-86c8254fb8df
# exit 0
```

stdout:
```
Following Deployment #55; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
applied: web deployed, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #55 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### deployment start --json a queued Deployment

```console
$ ployz --json deployment start a87c5ece-3950-4ce6-b210-23cb291b2dab
# exit 0
```

stdout:
```
{
  "admitted_at": 1791166987,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ended_at": 1791166990,
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 35
  },
  "environment_id": "4b4e5354-8834-4aea-a807-e65f832680bc",
  "id": "a87c5ece-3950-4ce6-b210-23cb291b2dab",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show a87c5ece-3950-4ce6-b210-23cb291b2dab",
  "nodes": [
    {
      "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
      "name": "web",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "a48be9de-5046-4ce1-ac09-736234a707d0",
      "name": "api",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
      "name": "postgres",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
      "name": "worker",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "71802251-dbfa-42fa-9bc9-895e8385ece6",
      "name": "pg-data",
      "outcome": "unchanged",
      "type": "volume"
    }
  ],
  "number": 57,
  "outcome": {
    "reason": null,
    "summary": {
      "completed": 0,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [],
    "would_remove": []
  },
  "remove": false,
  "runner": "cloud-01M44XYFPSFWBENTK55W52FDW6",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 25,
  "services": [],
  "started_at": 1791166989,
  "status": "applied",
  "upload": null
}
```

stderr:
```
Following Deployment #57; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
applied: web unchanged, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
```

### deployment cancel a running Deployment

```console
$ ployz deployment cancel 172c95d8-2a76-4faf-adc6-b56faf8ffe54
# exit 0
```

stdout:
```
Deployment #58 of shop/production: cancelling
  web: pending
  api: pending
  postgres: pending
  worker: pending
  pg-data: pending
```

### deployment show of a cancelled Deployment

```console
$ ployz deployment show 172c95d8-2a76-4faf-adc6-b56faf8ffe54
# exit 0
```

stdout:
```
Deployment #58 of shop/production: cancelled
  Cancelled before it executed
  web: not attempted
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### deployment retry of a cancelled Deployment

```console
$ ployz deployment retry 172c95d8-2a76-4faf-adc6-b56faf8ffe54
# exit 0
```

stdout:
```
Following Deployment #59; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
applied: web deployed, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #59 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

## up from a directory with index.html

### up --help

```console
$ ployz up --help
# exit 0
```

stdout:
```
Deploy this directory: create and link its Project if needed, upload, build and deploy it

Usage: ployz up [OPTIONS]

Options:
      --connect <connect>
          [env: PLOYZ_CONNECT=]

      --ssh-timeout <SECONDS>
          SSH setup timeout in seconds. Management commands use noninteractive authentication. During provisioning, only the network connection is timed; SSH/sudo authentication and installer execution have no deadline.
          
          [default: 20]

      --ployz-config <ployz-config>
          [env: PLOYZ_CONFIG=<run-dir>/private/config.yaml]
          [default: ~/.config/ployz/config.yaml]

  -c, --context <context>
          [env: PLOYZ_CONTEXT=]

      --project <project>
          Project [default: the only Project]
          
          [env: PLOYZ_PROJECT=]

      --env <env>
          Environment [default: the Project's Default Environment]
          
          [env: PLOYZ_ENV=]

      --json
          Print the result as one JSON object on stdout

      --events <FILE>
          Also write progress to FILE as NDJSON

      --detach
          Return the queued Deployment at once instead of following it

      --server <USER@HOST>
          First add this Server over SSH, as `ployz server add` does; the first founds the Cluster

      --reset
          Reset the --server if it already runs Ployz, before adding it

      --accept-volume-loss <name>
          Accept permanent deletion: repeat once per exact volume name in the full deletion list; --yes cannot bypass this

      --expect-version <VERSION>
          Refuse unless this is still the reviewed version; a data-loss refusal names it

  -h, --help
          Print help (see a summary with '-h')
```

### up (pipe)

```console
$ ployz up
# exit 0
```

stdout:
```
Created Project workspace.
Added Service workspace.
Following Deployment #1; stopping this leaves it running.
queued: workspace pending
running: workspace pending
applied: workspace deployed
Deployment #1 of workspace/production: applied
  uploaded by Ada Lovelace, base 5a3ec95
  workspace: deployed
  build workspace from the upload: built 
Open https://workspace.ada.ployz.test
Dashboard: http://localhost:44581/cloud/ada/workspace/production
```

### up after a change (live follower) (TTY)

```console
$ ployz up
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Following Deployment #2; stopping this leaves it running.
queued: workspace pending
running: workspace pending
applied: workspace deployed
Deployment #2 of workspace/production: applied
  uploaded by Ada Lovelace, base 5a3ec95
  workspace: deployed
  build workspace from the upload: built 
Open https://workspace.ada.ployz.test
Dashboard: http://localhost:44581/cloud/ada/workspace/production
# exit 0

```

### up --json

```console
$ ployz --json up
# exit 0
```

stdout:
```
{
  "dashboard": "http://localhost:44581/cloud/ada/workspace/production",
  "deployment": {
    "admitted_at": 1791167937,
    "admitted_by": "Ada Lovelace",
    "builds": [
      {
        "commit": null,
        "message": null,
        "service": "workspace",
        "status": "reused"
      }
    ],
    "ended_at": 1791167939,
    "environment": {
      "id": "2a96e90a-547d-4cf0-b0ee-e4803ddc2134",
      "name": "production",
      "project": "workspace",
      "revision": 3
    },
    "environment_id": "2a96e90a-547d-4cf0-b0ee-e4803ddc2134",
    "id": "96812240-9c93-4eea-b78c-ac7826d364ce",
    "in_flight": false,
    "message": null,
    "namespace": "workspace-production",
    "nodes": [
      {
        "id": "2c77e1c4-78ac-4c74-89be-d2939e121d16",
        "name": "workspace",
        "outcome": "unchanged",
        "type": "service"
      }
    ],
    "number": 3,
    "outcome": {
      "reason": null,
      "summary": {
        "completed": 0,
        "type": "success"
      },
      "type": "executed"
    },
    "preview": {
      "namespace": "workspace-production",
      "operations": [],
      "preserved_volumes": [],
      "prune_refusal": null,
      "storage": [],
      "volumes_to_create": [],
      "warnings": [],
      "would_remove": []
    },
    "remove": false,
    "runner": "cloud-01M44YVFKGDFEND8NG8Q6QD37F",
    "runtime_names": {
      "workspace": "workspace"
    },
    "saved": 1,
    "services": [],
    "started_at": 1791167938,
    "status": "applied",
    "upload": {
      "base": {
        "changed": false,
        "commit": "5a3ec9550649d9ad1ffe1110a18df621dec5dc54"
      },
      "digest": "7e8b77a7b0f94ed04590160597f8880da84cb861dcc9e0322b84a51890149a77",
      "uploader": "Ada Lovelace"
    }
  },
  "directory": "<run-dir>/workspace",
  "next": "ployz deployment show 96812240-9c93-4eea-b78c-ac7826d364ce",
  "urls": [
    "https://workspace.ada.ployz.test"
  ]
}
```

stderr:
```
Following Deployment #3; stopping this leaves it running.
queued: workspace pending
running: workspace pending
applied: workspace unchanged
```

### status after up (two Projects)

```console
$ ployz status
# exit 0
```

stdout:
```
Cloud: http://localhost:44581
Organization ada (token).
Environment workspace/production.
Linked from <run-dir>/workspace.
0 staged changes.
```

### service ls after up (two Projects, no --project, linked directory)

```console
$ ployz service ls
# exit 0
```

stdout:
```
SERVICE	PRIVATE DNS	SOURCE	NEXT DEPLOY
workspace	workspace	uploaded	-
```

### deployment ls after up

```console
$ ployz deployment ls
# exit 0
```

stdout:
```
#3 applied Saved revision 1 96812240-9c93-4eea-b78c-ac7826d364ce
#2 applied Saved revision 1 f5f3b219-f317-450f-aab9-7eb6b36580fc
#1 applied Saved revision 1 b7baead9-dcef-439d-b2f4-48833055d56c
```

### project rm the up Project

```console
$ ployz project rm workspace --confirm workspace
# exit 0
```

stdout:
```
Following Deployment #4; stopping this leaves it running.
queued: workspace pending
running: workspace pending
applied: workspace removed
Took an Environment off the Servers (Deployment #4).
Removed Project workspace and its Environments (production).
```

### status with the directory still linked to the removed Project

```console
$ ployz status
# exit 0
```

stdout:
```
Cloud: http://localhost:44581
Organization ada (token).
Linked from <run-dir>/workspace.
Needs attention: No Project named workspace
```

### status --json with the directory still linked to the removed Project

```console
$ ployz --json status
# exit 0
```

stdout:
```
{
  "attention": [
    {
      "message": "No Project named workspace",
      "reason": "not_found"
    }
  ],
  "deploying": [],
  "environment": null,
  "identity": {
    "cloud": "http://localhost:44581",
    "credential": "token",
    "organization": {
      "id": "eccc0cff-2641-4745-8dac-fdee92352300",
      "slug": "ada"
    }
  },
  "scope": {
    "environment": {
      "name": "production",
      "source": "link"
    },
    "link": "<run-dir>/workspace",
    "project": {
      "name": "workspace",
      "source": "link"
    }
  },
  "staged": null
}
```

