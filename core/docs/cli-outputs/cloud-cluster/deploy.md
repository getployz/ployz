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
Decided by the Servers when it runs: which containers start and stop.
```

stderr:
```
next: ployz deploy --expect-version 14:1:0.1
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
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 14
  },
  "ingress": [],
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 14:1:0.1",
  "unresolved": [
    "operations"
  ],
  "version": "14:1:0.1"
}
```

### deploy, nothing staged

```console
$ ployz deploy
# exit 0
```

stderr:
```
Nothing changed on the Servers; Deployment #2 shipped version #1.
```

### deploy --json, nothing staged

```console
$ ployz --json deploy
# exit 0
```

stdout:
```
{
  "admitted_at": 1792535114,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535126,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 14
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "44ca2765-db3e-4033-aad9-953f48c34719",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 3",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 3,
  "outcome": {
    "cause": [],
    "reason": null,
    "services": [],
    "summary": {
      "completed": 0,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [
      [
        "api",
        "web"
      ],
      [
        "redis",
        "web"
      ],
      [
        "api",
        "worker"
      ],
      [
        "redis",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "redis": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [],
    "would_remove": []
  },
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DPPCKADMS4RNXE7958TKPZ",
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
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": 1792535121,
  "status": "applied",
  "upload": null
}
```

stderr:
```
Nothing changed on the Servers; Deployment #3 shipped version #1.
```

## Staged change

### stage a change

```console
$ ployz set web.env.GREETING=hello
# exit 0
```

stdout:
```
Staged web.env.GREETING in shop/production.
```

stderr:
```
next: ployz deploy
```

### diff

```console
$ ployz diff
# exit 0
```

stdout:
```
web (update)
  web.env.GREETING: null → "hello"
```

stderr:
```
next: ployz deploy
```

### deploy --plan with a staged change

```console
$ ployz deploy --plan
# exit 0
```

stdout:
```
web (update)
  web.env.GREETING: null → "hello"
Decided by the Servers when it runs: which containers start and stop.
```

stderr:
```
next: ployz deploy --expect-version 15:1:0.3
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
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "lifecycle": "update",
      "name": "web",
      "restarts": [],
      "row": "09c3b481-f282-404c-94db-6228ed20c6b8:node",
      "settings": [
        {
          "after": "hello",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "web.env.GREETING",
          "row": "09c3b481-f282-404c-94db-6228ed20c6b8:variables.GREETING"
        }
      ],
      "type": "service"
    }
  ],
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 15
  },
  "ingress": [],
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 15:1:0.3",
  "unresolved": [
    "operations"
  ],
  "version": "15:1:0.3"
}
```

### deploy a staged change (live follower) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Deploying #4 of shop/production
✔ Volume pg-data  unchanged
✔ Config sentry  unchanged
depth 0
  ✔ api  unchanged
  ✔ postgres  unchanged
  ✔ redis  unchanged
depth 1
  ✔ web  switched 1/1  14.0s
  ✔ worker  unchanged
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
# exit 0
```

### deploy a staged change (pipe)

```console
$ ployz deploy --message Say\ hi
# exit 0
```

stderr:
```
Deploying #5 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  web (depth 1): booting 0/1
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
```

### deploy --json a staged change

```console
$ ployz --json deploy
# exit 0
```

stdout:
```
{
  "admitted_at": 1792535205,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535232,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 17
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "e67539d8-742b-4f95-8b9e-bb33a2fc4dc2",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 6",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792535231,
          "history": [
            {
              "at": 1792535216,
              "state": "pending"
            },
            {
              "at": 1792535217,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792535218,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792535224,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792535231,
              "state": "completed"
            }
          ],
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "server": "machine-1",
          "started_at": 1792535217,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [
        {
          "finished_at": 1792535231,
          "history": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "server": "machine-1",
          "started_at": 1792535217,
          "state": "completed"
        }
      ],
      "type": "config"
    }
  ],
  "number": 6,
  "outcome": {
    "cause": [],
    "reason": null,
    "services": [
      {
        "result": "new",
        "service": "web"
      }
    ],
    "summary": {
      "completed": 2,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [
      [
        "api",
        "web"
      ],
      [
        "redis",
        "web"
      ],
      [
        "api",
        "worker"
      ],
      [
        "redis",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "redis": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": null,
        "index": 0,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "dependency": "shop-production/redis",
          "dependent": "shop-production/web",
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "type": "wait_healthy"
        },
        "service_name": "web",
        "status": {
          "type": "pending"
        }
      },
      {
        "display_name": "ployz-create-d494bbd62f55a2478748c89881cc106a361420f8854d99e54829f26a76186491",
        "index": 1,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "old_container_id": "317067d0e7c36b12dc6f087b4282e6cdfd3a07290b50ead03fcee843e15606be",
          "skip_health_monitor": false,
          "spec": {
            "configs": [
              {
                "content": [],
                "name": "sentry/config.yml"
              },
              {
                "content": [],
                "name": "sentry/sentry.conf.py"
              }
            ],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [
                {
                  "config_name": "sentry/config.yml",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/config.yml",
                  "uid": 0
                },
                {
                  "config_name": "sentry/sentry.conf.py",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/sentry.conf.py",
                  "uid": 0
                }
              ],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "state": "disabled"
              },
              "hostname": null,
              "image": "nginx:1.27-alpine",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "09c3b481-f282-404c-94db-6228ed20c6b8"
              },
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
            "service_id": "7e3d7155707640b689a0a1ffd3cdf13f",
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
        "message": "acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.",
        "type": "ingress_hostname"
      },
      {
        "message": "web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.",
        "type": "ingress_hostname"
      }
    ],
    "would_remove": []
  },
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DPS64FXE4X1FPN4WTKJ45B",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 4,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": 1792535212,
  "status": "applied",
  "upload": null
}
```

stderr:
```
Deploying #6 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): booting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
```

## Detach

### deploy --detach

```console
$ ployz deploy --detach
# exit 0
```

stdout:
```
Deployment #7 of shop/production: queued
  api: pending
  postgres: pending
  redis: pending
  web: pending
  worker: pending
  pg-data: pending
  sentry: pending
```

### deploy --detach --json

```console
$ ployz --json deploy --detach
# exit 0
```

stdout:
```
{
  "admitted_at": 1792535268,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": null,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 19
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "219bc4ac-e761-401e-a7eb-bc21c5edfd9f",
  "in_flight": true,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 8",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "pending",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "pending",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 8,
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
  "saved": 6,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
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
DEPLOYMENT	STATUS	SAVED REVISION
#8	applied	6
#7	applied	5
#6	applied	4
#5	applied	3
#4	applied	2
```

stderr:
```
next: ployz deployment ls --cursor NA --limit 5
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
      "admitted_at": 1792535268,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": 1792535295,
      "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
      "held": false,
      "id": "219bc4ac-e761-401e-a7eb-bc21c5edfd9f",
      "in_flight": false,
      "message": null,
      "number": 8,
      "outcome": {
        "cause": [],
        "reason": null,
        "services": [
          {
            "result": "new",
            "service": "web"
          }
        ],
        "summary": {
          "completed": 2,
          "type": "success"
        },
        "type": "executed"
      },
      "push": null,
      "remove": false,
      "runner": "cloud-01M5DPV3PE9FNZ21ERP74C7R1X",
      "saved": 6,
      "services": [],
      "ships": [
        "web",
        "postgres",
        "redis",
        "worker",
        "api"
      ],
      "started_at": 1792535275,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1792535236,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": 1792535264,
      "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
      "held": false,
      "id": "d687bef2-6be7-47ac-86ac-004913b5dccd",
      "in_flight": false,
      "message": null,
      "number": 7,
      "outcome": {
        "cause": [],
        "reason": null,
        "services": [
          {
            "result": "new",
            "service": "web"
          }
        ],
        "summary": {
          "completed": 2,
          "type": "success"
        },
        "type": "executed"
      },
      "push": null,
      "remove": false,
      "runner": "cloud-01M5DPT41T4FXB97GQGYTXR5PE",
      "saved": 5,
      "services": [],
      "ships": [
        "web",
        "postgres",
        "redis",
        "worker",
        "api"
      ],
      "started_at": 1792535242,
      "status": "applied",
      "upload": null
    }
  ],
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 19
  },
  "in_flight": [],
  "latest": {
    "admitted_at": 1792535268,
    "admitted_by": "Ada Lovelace",
    "ci": null,
    "ended_at": 1792535295,
    "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "held": false,
    "id": "219bc4ac-e761-401e-a7eb-bc21c5edfd9f",
    "in_flight": false,
    "message": null,
    "number": 8,
    "outcome": {
      "cause": [],
      "reason": null,
      "services": [
        {
          "result": "new",
          "service": "web"
        }
      ],
      "summary": {
        "completed": 2,
        "type": "success"
      },
      "type": "executed"
    },
    "push": null,
    "remove": false,
    "runner": "cloud-01M5DPV3PE9FNZ21ERP74C7R1X",
    "saved": 6,
    "services": [],
    "ships": [
      "web",
      "postgres",
      "redis",
      "worker",
      "api"
    ],
    "started_at": 1792535275,
    "status": "applied",
    "upload": null
  },
  "next": "ployz deployment ls --cursor Nw --limit 2",
  "next_cursor": "Nw"
}
```

### deployment show by id

```console
$ ployz deployment show 219bc4ac-e761-401e-a7eb-bc21c5edfd9f
# exit 0
```

stdout:
```
Deployment #8 of shop/production: applied
  web: serves its new copies
  api: unchanged
  postgres: unchanged
  redis: unchanged
  web: deployed
  worker: unchanged
  pg-data: unchanged
  sentry: unchanged
```

### deployment show by number

```console
$ ployz deployment show 8
# exit 0
```

stdout:
```
Deployment #8 of shop/production: applied
  web: serves its new copies
  api: unchanged
  postgres: unchanged
  redis: unchanged
  web: deployed
  worker: unchanged
  pg-data: unchanged
  sentry: unchanged
```

### deployment show --json

```console
$ ployz --json deployment show 219bc4ac-e761-401e-a7eb-bc21c5edfd9f
# exit 0
```

stdout:
```
{
  "admitted_at": 1792535268,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535295,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 19
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "219bc4ac-e761-401e-a7eb-bc21c5edfd9f",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792535293,
          "history": [
            {
              "at": 1792535279,
              "state": "pending"
            },
            {
              "at": 1792535280,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792535281,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792535286,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792535293,
              "state": "completed"
            }
          ],
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "server": "machine-1",
          "started_at": 1792535280,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [
        {
          "finished_at": 1792535293,
          "history": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "server": "machine-1",
          "started_at": 1792535280,
          "state": "completed"
        }
      ],
      "type": "config"
    }
  ],
  "number": 8,
  "outcome": {
    "cause": [],
    "reason": null,
    "services": [
      {
        "result": "new",
        "service": "web"
      }
    ],
    "summary": {
      "completed": 2,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [
      [
        "api",
        "web"
      ],
      [
        "redis",
        "web"
      ],
      [
        "api",
        "worker"
      ],
      [
        "redis",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "redis": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": null,
        "index": 0,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "dependency": "shop-production/redis",
          "dependent": "shop-production/web",
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "type": "wait_healthy"
        },
        "service_name": "web",
        "status": {
          "type": "pending"
        }
      },
      {
        "display_name": "ployz-create-3f955d386ade2d26826e2211260c1a4d22260edb19eca7923b922f78c8088294",
        "index": 1,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "old_container_id": "fd156f3aa2984cd316fae22b6ab96c55037dedc4be27bc540b1707c6bb2df46f",
          "skip_health_monitor": false,
          "spec": {
            "configs": [
              {
                "content": [],
                "name": "sentry/config.yml"
              },
              {
                "content": [],
                "name": "sentry/sentry.conf.py"
              }
            ],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [
                {
                  "config_name": "sentry/config.yml",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/config.yml",
                  "uid": 0
                },
                {
                  "config_name": "sentry/sentry.conf.py",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/sentry.conf.py",
                  "uid": 0
                }
              ],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "state": "disabled"
              },
              "hostname": null,
              "image": "nginx:1.27-alpine",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "09c3b481-f282-404c-94db-6228ed20c6b8"
              },
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
            "service_id": "7e3d7155707640b689a0a1ffd3cdf13f",
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
        "message": "acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.",
        "type": "ingress_hostname"
      },
      {
        "message": "web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.",
        "type": "ingress_hostname"
      }
    ],
    "would_remove": []
  },
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DPV3PE9FNZ21ERP74C7R1X",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 6,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": 1792535275,
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
Staged worker.image in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy that fails

```console
$ ployz deploy
# exit 1
```

stderr:
```
Deploying #9 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): waiting 0/1
  Volume pg-data: unchanged
  Config sentry: pending
  worker (depth 1): failed (0/1 ready)
  Config sentry: failed
────────────────────────────────────────
5 unchanged · 2 failed · across 1 server
error: worker: pull image failed.
  cause: Docker responded with status code 500: error from registry: denied; denied
inspect: ployz get worker.image --project shop --env production
undo: ployz history undo 7 --project shop --env production
```

### deploy that fails (live follower) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Deploying #10 of shop/production
✔ Volume pg-data  unchanged
✘ Config sentry  failed
depth 0
  ✔ api  unchanged
  ✔ postgres  unchanged
  ✔ redis  unchanged
depth 1
  ✔ web  unchanged
  ✘ worker  failed (0/1 ready)  0.0s
────────────────────────────────────────
5 unchanged · 2 failed · across 1 server
error: worker: pull image failed.
  cause: Docker responded with status code 500: error from registry: denied; denied
inspect: ployz get worker.image --project shop --env production
undo: ployz history undo 7 --project shop --env production
# exit 1
```

### deploy --json that fails

```console
$ ployz --json deploy
# exit 1
```

stdout:
```
{
  "admitted_at": 1792535338,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535350,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 20
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "0cbdcade-f28a-40da-80d2-a5ae7b2d4187",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 11",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792535349,
          "history": [
            {
              "at": 1792535348,
              "state": "pending"
            },
            {
              "at": 1792535349,
              "cause": [
                "image pull failed",
                "Docker responded with status code 500: error from registry: denied\ndenied"
              ],
              "log": null,
              "reason": "pull image failed",
              "state": "failed",
              "step": "pull"
            }
          ],
          "log": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792535349,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792535349,
          "history": null,
          "log": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792535349,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "config"
    }
  ],
  "number": 11,
  "outcome": {
    "cause": [
      "image pull failed",
      "Docker responded with status code 500: error from registry: denied\ndenied"
    ],
    "reason": "worker: pull image failed",
    "services": [
      {
        "result": "not_started",
        "service": "worker"
      }
    ],
    "summary": {
      "completed": 0,
      "reason": "machine",
      "type": "failed",
      "unexecuted": 1
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [
      [
        "api",
        "web"
      ],
      [
        "redis",
        "web"
      ],
      [
        "api",
        "worker"
      ],
      [
        "redis",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "redis": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": null,
        "index": 0,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "dependency": "shop-production/redis",
          "dependent": "shop-production/worker",
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "type": "wait_healthy"
        },
        "service_name": "worker",
        "status": {
          "type": "pending"
        }
      },
      {
        "display_name": "ployz-create-34f34f0bd755459b583b831bdbb54703e4df1b0405adce1d2dc6590f777f26a1",
        "index": 1,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "old_container_id": "4b505bd220e2799ec32cb9f9e14c0eac8440e3f7762c20fbe186ef1a81171dd7",
          "skip_health_monitor": false,
          "spec": {
            "configs": [
              {
                "content": [],
                "name": "sentry/config.yml"
              },
              {
                "content": [],
                "name": "sentry/sentry.conf.py"
              }
            ],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [
                {
                  "config_name": "sentry/config.yml",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/config.yml",
                  "uid": 0
                },
                {
                  "config_name": "sentry/sentry.conf.py",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/sentry.conf.py",
                  "uid": 0
                }
              ],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "path": "/health",
                "port": 80,
                "state": "http",
                "timeout_seconds": 30
              },
              "hostname": null,
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1"
              },
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
            "service_id": "61c4c34299b24b9480bf9cf96921ddec",
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
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DPX7YQ06QR226T1A1SQJDC",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 7,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": 1792535345,
  "status": "failed",
  "upload": null
}
```

stderr:
```
Deploying #11 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): waiting 0/1
  Volume pg-data: unchanged
  Config sentry: pending
  worker (depth 1): failed (0/1 ready)
  Config sentry: failed
────────────────────────────────────────
5 unchanged · 2 failed · across 1 server
error: worker: pull image failed.
  cause: Docker responded with status code 500: error from registry: denied; denied
inspect: ployz get worker.image --project shop --env production
undo: ployz history undo 7 --project shop --env production
```

### deployment show of the failed Deployment

```console
$ ployz deployment show 0cbdcade-f28a-40da-80d2-a5ae7b2d4187
# exit 0
```

stdout:
```
Deployment #11 of shop/production: failed
  worker: pull image failed
    cause: Docker responded with status code 500: error from registry: denied
denied
  worker: not started
  api: unchanged
  postgres: unchanged
  redis: unchanged
  web: unchanged
  worker: failed
  pg-data: unchanged
  sentry: failed
```

### deployment show --json of the failed Deployment

```console
$ ployz --json deployment show 0cbdcade-f28a-40da-80d2-a5ae7b2d4187
# exit 0
```

stdout:
```
{
  "admitted_at": 1792535338,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535350,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 20
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "0cbdcade-f28a-40da-80d2-a5ae7b2d4187",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792535349,
          "history": [
            {
              "at": 1792535348,
              "state": "pending"
            },
            {
              "at": 1792535349,
              "cause": [
                "image pull failed",
                "Docker responded with status code 500: error from registry: denied\ndenied"
              ],
              "log": null,
              "reason": "pull image failed",
              "state": "failed",
              "step": "pull"
            }
          ],
          "log": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792535349,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792535349,
          "history": null,
          "log": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792535349,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "config"
    }
  ],
  "number": 11,
  "outcome": {
    "cause": [
      "image pull failed",
      "Docker responded with status code 500: error from registry: denied\ndenied"
    ],
    "reason": "worker: pull image failed",
    "services": [
      {
        "result": "not_started",
        "service": "worker"
      }
    ],
    "summary": {
      "completed": 0,
      "reason": "machine",
      "type": "failed",
      "unexecuted": 1
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [
      [
        "api",
        "web"
      ],
      [
        "redis",
        "web"
      ],
      [
        "api",
        "worker"
      ],
      [
        "redis",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "redis": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": null,
        "index": 0,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "dependency": "shop-production/redis",
          "dependent": "shop-production/worker",
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "type": "wait_healthy"
        },
        "service_name": "worker",
        "status": {
          "type": "pending"
        }
      },
      {
        "display_name": "ployz-create-34f34f0bd755459b583b831bdbb54703e4df1b0405adce1d2dc6590f777f26a1",
        "index": 1,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "old_container_id": "4b505bd220e2799ec32cb9f9e14c0eac8440e3f7762c20fbe186ef1a81171dd7",
          "skip_health_monitor": false,
          "spec": {
            "configs": [
              {
                "content": [],
                "name": "sentry/config.yml"
              },
              {
                "content": [],
                "name": "sentry/sentry.conf.py"
              }
            ],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [
                {
                  "config_name": "sentry/config.yml",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/config.yml",
                  "uid": 0
                },
                {
                  "config_name": "sentry/sentry.conf.py",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/sentry.conf.py",
                  "uid": 0
                }
              ],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "path": "/health",
                "port": 80,
                "state": "http",
                "timeout_seconds": 30
              },
              "hostname": null,
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1"
              },
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
            "service_id": "61c4c34299b24b9480bf9cf96921ddec",
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
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DPX7YQ06QR226T1A1SQJDC",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 7,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": 1792535345,
  "status": "failed",
  "upload": null
}
```

### deployment redeploy of the failed Deployment

```console
$ ployz deployment redeploy 0cbdcade-f28a-40da-80d2-a5ae7b2d4187
# exit 1
```

stderr:
```
Deploying #12 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): waiting 0/1
  Volume pg-data: unchanged
  Config sentry: pending
  worker (depth 1): failed (0/1 ready)
  Config sentry: failed
────────────────────────────────────────
5 unchanged · 2 failed · across 1 server
error: worker: pull image failed.
  cause: Docker responded with status code 500: error from registry: denied; denied
inspect: ployz get worker.image --project shop --env production
undo: ployz history undo 7 --project shop --env production
```

### deployment redeploy --json of the failed Deployment

```console
$ ployz --json deployment redeploy 103ae67d-8002-4f1f-aafd-1138ae5dd36f
# exit 1
```

stdout:
```
{
  "admitted_at": 1792535371,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535384,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 20
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "86984325-59b9-4cba-90fb-13bd058972cb",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 13",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792535382,
          "history": [
            {
              "at": 1792535381,
              "state": "pending"
            },
            {
              "at": 1792535382,
              "cause": [
                "image pull failed",
                "Docker responded with status code 500: error from registry: denied\ndenied"
              ],
              "log": null,
              "reason": "pull image failed",
              "state": "failed",
              "step": "pull"
            }
          ],
          "log": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792535382,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792535382,
          "history": null,
          "log": null,
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792535382,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "config"
    }
  ],
  "number": 13,
  "outcome": {
    "cause": [
      "image pull failed",
      "Docker responded with status code 500: error from registry: denied\ndenied"
    ],
    "reason": "worker: pull image failed",
    "services": [
      {
        "result": "not_started",
        "service": "worker"
      }
    ],
    "summary": {
      "completed": 0,
      "reason": "machine",
      "type": "failed",
      "unexecuted": 1
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [
      [
        "api",
        "web"
      ],
      [
        "redis",
        "web"
      ],
      [
        "api",
        "worker"
      ],
      [
        "redis",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "redis": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": null,
        "index": 0,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "dependency": "shop-production/redis",
          "dependent": "shop-production/worker",
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "type": "wait_healthy"
        },
        "service_name": "worker",
        "status": {
          "type": "pending"
        }
      },
      {
        "display_name": "ployz-create-34f34f0bd755459b583b831bdbb54703e4df1b0405adce1d2dc6590f777f26a1",
        "index": 1,
        "machine_id": "19369a0e56924a53b1d32dd1a756d582",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "19369a0e56924a53b1d32dd1a756d582",
          "old_container_id": "4b505bd220e2799ec32cb9f9e14c0eac8440e3f7762c20fbe186ef1a81171dd7",
          "skip_health_monitor": false,
          "spec": {
            "configs": [
              {
                "content": [],
                "name": "sentry/config.yml"
              },
              {
                "content": [],
                "name": "sentry/sentry.conf.py"
              }
            ],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [
                {
                  "config_name": "sentry/config.yml",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/config.yml",
                  "uid": 0
                },
                {
                  "config_name": "sentry/sentry.conf.py",
                  "gid": 0,
                  "mode": 292,
                  "target": "/etc/sentry/sentry.conf.py",
                  "uid": 0
                }
              ],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "path": "/health",
                "port": 80,
                "state": "http",
                "timeout_seconds": 30
              },
              "hostname": null,
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1"
              },
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
            "service_id": "61c4c34299b24b9480bf9cf96921ddec",
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
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DPY7BB8CZH8E1B4BGSQA82",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 7,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": 1792535378,
  "status": "failed",
  "upload": null
}
```

stderr:
```
Deploying #13 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): failed (0/1 ready)
  Volume pg-data: unchanged
  Config sentry: pending
  Config sentry: failed
────────────────────────────────────────
5 unchanged · 2 failed · across 1 server
error: worker: pull image failed.
  cause: Docker responded with status code 500: error from registry: denied; denied
inspect: ployz get worker.image --project shop --env production
undo: ployz history undo 7 --project shop --env production
```

### fix the image

```console
$ ployz set worker.image=traefik/whoami:v1.10.3
# exit 0
```

stdout:
```
Staged worker.image in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy the fix

```console
$ ployz deploy
# exit 0
```

stderr:
```
Nothing changed on the Servers; Deployment #14 shipped version #8.
```

## Queued Deployments: cancel and start

### deployment ls with queued Deployments

```console
$ ployz deployment ls --limit 4
# exit 0
```

stdout:
```
DEPLOYMENT	STATUS	SAVED REVISION
#17	queued	10
#16	superseded	10
#15	superseded	9
#14	applied	8
```

stderr:
```
next: ployz deployment ls --cursor MTQ --limit 4
```

### deployment cancel a queued Deployment

```console
$ ployz deployment cancel bd92f5c5-9a6e-40f5-bdbb-3b3c9c3c1cfa
# exit 0
```

stdout:
```
Deployment #17 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  redis: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment cancel --json a queued Deployment

```console
$ ployz --json deployment cancel 92da1706-d217-46d3-a6e4-519e9568c400
# exit 0
```

stdout:
```
{
  "admitted_at": 1792535409,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535410,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 23
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "92da1706-d217-46d3-a6e4-519e9568c400",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 19",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "not_attempted",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "not_attempted",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 19,
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
  "saved": 10,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": null,
  "status": "cancelled",
  "upload": null
}
```

### deployment start a queued Deployment

```console
$ ployz deployment start d808a8e5-1c29-41c1-82b7-7ea6682dcd74
# exit 0
```

stderr:
```
Deploying #21 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  web (depth 1): booting 0/1
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
```

### deployment start --json a queued Deployment

```console
$ ployz --json deployment start 841ebbf2-9f67-49f6-98ba-e789b0b06e19
# exit 0
```

stdout:
```
{
  "admitted_at": 1792535452,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792535466,
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 23
  },
  "environment_id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
  "held": false,
  "id": "841ebbf2-9f67-49f6-98ba-e789b0b06e19",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 23",
  "nodes": [
    {
      "id": "e8e4727a-c3d8-4cbe-949e-fdd4f79f356d",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "44e8a505-5acb-4256-a0f0-072faef96989",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "52d44f81-5a85-4649-8c2f-2124d13ce2fd",
      "name": "redis",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "09c3b481-f282-404c-94db-6228ed20c6b8",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "811a8538-21d8-4859-9e45-6f7e3e51f7a1",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e0e06b7c-b642-449a-8185-91ebcf01ff50",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "d450e2a0-6b0c-4c54-b82a-78430277986a",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 23,
  "outcome": {
    "cause": [],
    "reason": null,
    "services": [],
    "summary": {
      "completed": 0,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [
      [
        "api",
        "web"
      ],
      [
        "redis",
        "web"
      ],
      [
        "api",
        "worker"
      ],
      [
        "redis",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "redis": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [],
    "preserved_volumes": [],
    "prune_refusal": null,
    "storage": [],
    "volumes_to_create": [],
    "warnings": [],
    "would_remove": []
  },
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DQ0QDFYX1R18KWSEK2A1A5",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "redis": "redis",
    "web": "web",
    "worker": "worker"
  },
  "saved": 10,
  "services": [],
  "ships": [
    "web",
    "postgres",
    "redis",
    "worker",
    "api"
  ],
  "started_at": 1792535461,
  "status": "applied",
  "upload": null
}
```

stderr:
```
Nothing changed on the Servers; Deployment #23 shipped version #10.
```

### deployment cancel a running Deployment

```console
$ ployz deployment cancel d205138a-b6d6-4d49-95da-4916ba82c485
# exit 0
```

stdout:
```
Deployment #24 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  redis: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment show of a cancelled Deployment

```console
$ ployz deployment show d205138a-b6d6-4d49-95da-4916ba82c485
# exit 0
```

stdout:
```
Deployment #24 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  redis: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment redeploy of a cancelled Deployment

```console
$ ployz deployment redeploy d205138a-b6d6-4d49-95da-4916ba82c485
# exit 0
```

stderr:
```
Deploying #25 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  web (depth 1): booting 0/1
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
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
          Reach the Servers through this connection instead of Cloud or a context
          
          [env: PLOYZ_CONNECT=]

      --ssh-timeout <SECONDS>
          SSH setup timeout in seconds. Management commands use noninteractive authentication. During provisioning, only the network connection is timed; SSH/sudo authentication and installer execution have no deadline.
          
          [default: 20]

  -c, --context <context>
          Act through this local context instead of the current one
          
          [env: PLOYZ_CONTEXT=]

      --project <project>
          Project [default: the only Project]
          
          [env: PLOYZ_PROJECT=]

      --env <env>
          Environment [default: the Project's Default Environment]
          
          [env: PLOYZ_ENV=]

      --json
          Print the result as one JSON object on stdout; streams print one per line

      --color <WHEN>
          When to color output; overrides NO_COLOR and CLICOLOR
          
          [default: auto]
          [possible values: auto, always, never]

      --events <FILE>
          Also write progress to FILE as NDJSON

      --detach
          Return the queued Deployment at once instead of following it

      --server <USER@HOST>
          First add this Server over SSH, as `ployz server add` does; the first founds the Cluster

      --reset
          Reset the --server if it already runs Ployz, before adding it

      --accept-volume-loss <name>
          Accept permanent deletion: repeat once per exact volume name in the full deletion list

      --expect-version <VERSION>
          Refuse unless this is still the reviewed version; a data-loss refusal names it

      --approval <ID>
          Go ahead with the approval a human gave in Ployz Cloud; an approval refusal names it

  -h, --help
          Print help (see a summary with '-h')

Docs: ployz docs cli/overview.md
```

### up (pipe)

```console
$ ployz up
# exit 0
```

stdout:
```
Open https://workspace.ada.ployz.test
Dashboard: http://localhost:33907/cloud/ada/workspace/production
```

stderr:
```
Created Project workspace.
Added Service workspace.
Deploying #1 of workspace/production
  workspace: waiting (building)
  workspace: waiting (building on machine-1)
  still waiting: workspace (waiting)
  workspace: waiting (built on machine-1)
  workspace (depth 0): waiting 0/1
! workspace.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  workspace (depth 0): booting 0/1
  workspace (depth 0): switching 1/1
  workspace (depth 0): switched 1/1
────────────────────────────
1 deployed · across 1 server
workspace built on machine-1.
```

### up after a change (live follower) (TTY)

```console
$ ployz up
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Deploying #2 of workspace/production
depth 0
  ✔ workspace  switched 1/1  23.0s
! workspace.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
────────────────────────────
1 deployed · across 1 server
workspace built on machine-1.
Open https://workspace.ada.ployz.test
Dashboard: http://localhost:33907/cloud/ada/workspace/production
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
  "dashboard": "http://localhost:33907/cloud/ada/workspace/production",
  "deployment": {
    "admitted_at": 1792536591,
    "admitted_by": "Ada Lovelace",
    "builds": [
      {
        "commit": null,
        "finished_at": 1792536600,
        "github_skipped": null,
        "message": null,
        "server": null,
        "service": "workspace",
        "started_at": 1792536599,
        "status": "reused"
      }
    ],
    "ci": null,
    "ended_at": 1792536604,
    "environment": {
      "id": "850a7afb-4ee0-47f4-bf83-08eaee011c8f",
      "name": "production",
      "project": "workspace",
      "revision": 3
    },
    "environment_id": "850a7afb-4ee0-47f4-bf83-08eaee011c8f",
    "held": false,
    "id": "357dc69b-d937-4eab-93e6-2cdf38324ed3",
    "in_flight": false,
    "message": null,
    "namespace": "workspace-production",
    "nodes": [
      {
        "id": "d4be0a45-67c2-44a9-a6bf-43267f33f712",
        "name": "workspace",
        "outcome": "unchanged",
        "rows": [],
        "type": "service"
      }
    ],
    "number": 3,
    "outcome": {
      "cause": [],
      "reason": null,
      "services": [],
      "summary": {
        "completed": 0,
        "type": "success"
      },
      "type": "executed"
    },
    "preview": {
      "dependencies": [],
      "depths": {
        "workspace": 0
      },
      "namespace": "workspace-production",
      "operations": [],
      "preserved_volumes": [],
      "prune_refusal": null,
      "storage": [],
      "volumes_to_create": [],
      "warnings": [],
      "would_remove": []
    },
    "push": null,
    "remove": false,
    "rolls_back": [],
    "runner": "cloud-01M5DR3FE41W02NH4G1YBTC82J",
    "runtime_names": {
      "workspace": "workspace"
    },
    "saved": 1,
    "services": [],
    "ships": [
      "workspace"
    ],
    "started_at": 1792536598,
    "status": "applied",
    "upload": {
      "base": null,
      "digest": "7e8b77a7b0f94ed04590160597f8880da84cb861dcc9e0322b84a51890149a77",
      "uploader": "Ada Lovelace"
    }
  },
  "directory": "<run-dir>/workspace",
  "next": "ployz deployment show 3",
  "urls": [
    "https://workspace.ada.ployz.test"
  ]
}
```

stderr:
```
Nothing changed on the Servers; Deployment #3 shipped version #1.
```

### status after up (two Projects)

```console
$ ployz status
# exit 0
```

stdout:
```
cloud = http://localhost:33907
organization = ada
signed in as = token
environment = workspace/production
linked from = <run-dir>/workspace
staged = 0 changes
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
DEPLOYMENT	STATUS	SAVED REVISION
#3	applied	1
#2	applied	1
#1	applied	1
```

### project rm the up Project (Ada approves it)

```console
$ ployz project rm workspace --confirm workspace
# exit 0
```

stdout:
```
Removed Project workspace and its Environments (production).
```

stderr:
```
Removing workspace removes Service workspace.
This removal in Organization ada destroys 1 thing:
  ✕ removes Service workspace
Approve 2e0299a9-9138-45db-9ff0-a320e1bb31e4 under Waiting on you in the Ployz agent at http://localhost:33907/cloud/ada?chat=ad316458-6a3c-48e5-8f3b-2d6de896b758
next: ployz org approval approve 2e0299a9-9138-45db-9ff0-a320e1bb31e4
Waiting for approval in Ployz Cloud… approved.
Deploying #4 of workspace/production
  workspace: removing 0/1
  workspace: removed 1/1
```

### status with the directory still linked to the removed Project

```console
$ ployz status
# exit 0
```

stdout:
```
cloud = http://localhost:33907
organization = ada
signed in as = token
linked from = <run-dir>/workspace
needs attention = No Project named workspace
```

stderr:
```
next: ployz link --project PROJECT
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
      "next": "ployz link --project PROJECT",
      "reason": "not_found"
    }
  ],
  "deploying": [],
  "environment": null,
  "identity": {
    "cloud": "http://localhost:33907",
    "credential": "token",
    "organization": {
      "id": "8e39d474-d16a-403d-929b-2c0e4c4447b2",
      "slug": "ada"
    }
  },
  "next": "ployz link --project PROJECT",
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

