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
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
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
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### deploy --json, nothing staged

```console
$ ployz --json deploy
# exit 0
```

stdout:
```
{
  "admitted_at": 1792529559,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529571,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 14
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "dd80e5a2-069f-431f-8afd-d113ef2539e4",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 3",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
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
        "api",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
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
  "runner": "cloud-01M5DHCVXQ0T0H6CXPN54ZWDAZ",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 1,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": 1792529566,
  "status": "applied",
  "upload": null,
  "warnings": [
    "Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.",
    "Left PORT empty in sentry/config.yml: it is a broken reference."
  ]
}
```

stderr:
```
Nothing changed on the Servers; Deployment #3 shipped version #1.
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
sentry/config.yml references redis, which was removed; deploys skip it until you change it: ployz config put sentry config.yml --from PATH
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
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "lifecycle": "update",
      "name": "web",
      "restarts": [],
      "row": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636:node",
      "settings": [
        {
          "after": "hello",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "web.env.GREETING",
          "row": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636:variables.GREETING"
        }
      ],
      "type": "service"
    }
  ],
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
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
depth 1
  ✔ web  switched 1/1  15.0s
  ✔ worker  unchanged
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web's image nginx:1.27-alpine exposes 80; its domains send to 8080. Set the domain port or PORT.
──────────────────────────────────────────
1 deployed · 5 unchanged · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
  web (depth 1): booting 0/1
! web's image nginx:1.27-alpine exposes 80; its domains send to 8080. Set the domain port or PORT.
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 5 unchanged · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### deploy --json a staged change

```console
$ ployz --json deploy
# exit 0
```

stdout:
```
{
  "admitted_at": 1792529649,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529677,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 17
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "03af7b3d-ee04-4055-9e30-a9ef420c6113",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 6",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792529674,
          "history": [
            {
              "at": 1792529660,
              "state": "pending"
            },
            {
              "at": 1792529661,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792529661,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792529667,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792529674,
              "state": "completed"
            }
          ],
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "server": "machine-1",
          "started_at": 1792529661,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [
        {
          "finished_at": 1792529674,
          "history": null,
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "server": "machine-1",
          "started_at": 1792529661,
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
      "completed": 1,
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
        "api",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "ployz-create-315ff0e3a3a29b2d60093e9ee8fad37c7d2526949b40e971e9b4bc20ba074e3f",
        "index": 0,
        "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "old_container_id": "7c63a66b74056a35ea99f3f85d580e07ae31056937582ae7e3c4242a91991ec8",
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
                "cloud.ployz.service.id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636"
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
              }
            ],
            "pre_deploy": null,
            "service_id": "efd64e3b7a174e83bb1752008730a5d8",
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
      }
    ],
    "would_remove": []
  },
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DHFKXRMMM9V2KCWDR97QWX",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 4,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": 1792529656,
  "status": "applied",
  "upload": null,
  "warnings": [
    "Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.",
    "Left PORT empty in sentry/config.yml: it is a broken reference."
  ]
}
```

stderr:
```
Deploying #6 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  web (depth 1): booting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web's image nginx:1.27-alpine exposes 80; its domains send to 8080. Set the domain port or PORT.
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 5 unchanged · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
  "admitted_at": 1792529710,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": null,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 19
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "211132da-b5f8-4aa6-b0c2-128609292e36",
  "in_flight": true,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 8",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "pending",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
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
    "web": "web",
    "worker": "worker"
  },
  "saved": 6,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
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
      "admitted_at": 1792529710,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": 1792529740,
      "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
      "held": false,
      "id": "211132da-b5f8-4aa6-b0c2-128609292e36",
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
          "completed": 1,
          "type": "success"
        },
        "type": "executed"
      },
      "push": null,
      "remove": false,
      "runner": "cloud-01M5DHHFZSBQ9EY61CR5TV7C6G",
      "saved": 6,
      "services": [],
      "ships": [
        "api",
        "web",
        "worker",
        "postgres"
      ],
      "started_at": 1792529718,
      "status": "applied",
      "upload": null,
      "warnings": [
        {
          "broken": true,
          "config": "sentry",
          "file": "config.yml",
          "secret": "PLOYZ_PRIVATE_DOMAIN"
        },
        {
          "broken": true,
          "config": "sentry",
          "file": "config.yml",
          "secret": "PORT"
        },
        {
          "warning": {
            "exposed": [
              80
            ],
            "image": "nginx:1.27-alpine",
            "machines": [
              "97d7ad4b93fc402d9b5433d23f9e9fef"
            ],
            "routed": [
              8080
            ],
            "service": "web",
            "service_id": "efd64e3b7a174e83bb1752008730a5d8",
            "type": "image_port_mismatch"
          }
        }
      ]
    },
    {
      "admitted_at": 1792529678,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": 1792529707,
      "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
      "held": false,
      "id": "9500df1c-4983-44f3-bb1f-680d351bf08e",
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
          "completed": 1,
          "type": "success"
        },
        "type": "executed"
      },
      "push": null,
      "remove": false,
      "runner": "cloud-01M5DHGG4WCNQKTKEB5KXECH2S",
      "saved": 5,
      "services": [],
      "ships": [
        "api",
        "web",
        "worker",
        "postgres"
      ],
      "started_at": 1792529687,
      "status": "applied",
      "upload": null,
      "warnings": [
        {
          "broken": true,
          "config": "sentry",
          "file": "config.yml",
          "secret": "PLOYZ_PRIVATE_DOMAIN"
        },
        {
          "broken": true,
          "config": "sentry",
          "file": "config.yml",
          "secret": "PORT"
        },
        {
          "warning": {
            "exposed": [
              80
            ],
            "image": "nginx:1.27-alpine",
            "machines": [
              "97d7ad4b93fc402d9b5433d23f9e9fef"
            ],
            "routed": [
              8080
            ],
            "service": "web",
            "service_id": "efd64e3b7a174e83bb1752008730a5d8",
            "type": "image_port_mismatch"
          }
        }
      ]
    }
  ],
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 19
  },
  "in_flight": [],
  "latest": {
    "admitted_at": 1792529710,
    "admitted_by": "Ada Lovelace",
    "ci": null,
    "ended_at": 1792529740,
    "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "held": false,
    "id": "211132da-b5f8-4aa6-b0c2-128609292e36",
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
        "completed": 1,
        "type": "success"
      },
      "type": "executed"
    },
    "push": null,
    "remove": false,
    "runner": "cloud-01M5DHHFZSBQ9EY61CR5TV7C6G",
    "saved": 6,
    "services": [],
    "ships": [
      "api",
      "web",
      "worker",
      "postgres"
    ],
    "started_at": 1792529718,
    "status": "applied",
    "upload": null,
    "warnings": [
      {
        "broken": true,
        "config": "sentry",
        "file": "config.yml",
        "secret": "PLOYZ_PRIVATE_DOMAIN"
      },
      {
        "broken": true,
        "config": "sentry",
        "file": "config.yml",
        "secret": "PORT"
      },
      {
        "warning": {
          "exposed": [
            80
          ],
          "image": "nginx:1.27-alpine",
          "machines": [
            "97d7ad4b93fc402d9b5433d23f9e9fef"
          ],
          "routed": [
            8080
          ],
          "service": "web",
          "service_id": "efd64e3b7a174e83bb1752008730a5d8",
          "type": "image_port_mismatch"
        }
      }
    ]
  },
  "next": "ployz deployment ls --cursor Nw --limit 2",
  "next_cursor": "Nw"
}
```

### deployment show by id

```console
$ ployz deployment show 211132da-b5f8-4aa6-b0c2-128609292e36
# exit 0
```

stdout:
```
Deployment #8 of shop/production: applied
  web: serves its new copies
  api: unchanged
  postgres: unchanged
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
  web: deployed
  worker: unchanged
  pg-data: unchanged
  sentry: unchanged
```

### deployment show --json

```console
$ ployz --json deployment show 211132da-b5f8-4aa6-b0c2-128609292e36
# exit 0
```

stdout:
```
{
  "admitted_at": 1792529710,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529740,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 19
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "211132da-b5f8-4aa6-b0c2-128609292e36",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792529736,
          "history": [
            {
              "at": 1792529723,
              "state": "pending"
            },
            {
              "at": 1792529724,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792529724,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792529730,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792529736,
              "state": "completed"
            }
          ],
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "server": "machine-1",
          "started_at": 1792529724,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [
        {
          "finished_at": 1792529736,
          "history": null,
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "server": "machine-1",
          "started_at": 1792529724,
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
      "completed": 1,
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
        "api",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "ployz-create-e948ee1289090cb5c3e3dd337524d296faa1bfd22fdcca383b0acab83f029e38",
        "index": 0,
        "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "old_container_id": "5c8c90ea0c54f06572e6eb29213ba3463d507d129083e078edbb53877a7cc9e8",
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
                "cloud.ployz.service.id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636"
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
              }
            ],
            "pre_deploy": null,
            "service_id": "efd64e3b7a174e83bb1752008730a5d8",
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
      }
    ],
    "would_remove": []
  },
  "push": null,
  "remove": false,
  "rolls_back": [],
  "runner": "cloud-01M5DHHFZSBQ9EY61CR5TV7C6G",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 6,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": 1792529718,
  "status": "applied",
  "upload": null,
  "warnings": [
    {
      "broken": true,
      "config": "sentry",
      "file": "config.yml",
      "secret": "PLOYZ_PRIVATE_DOMAIN"
    },
    {
      "broken": true,
      "config": "sentry",
      "file": "config.yml",
      "secret": "PORT"
    },
    {
      "warning": {
        "exposed": [
          80
        ],
        "image": "nginx:1.27-alpine",
        "machines": [
          "97d7ad4b93fc402d9b5433d23f9e9fef"
        ],
        "routed": [
          8080
        ],
        "service": "web",
        "service_id": "efd64e3b7a174e83bb1752008730a5d8",
        "type": "image_port_mismatch"
      }
    }
  ]
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
  web (depth 1): unchanged
  worker (depth 1): waiting 0/1
  Volume pg-data: unchanged
  Config sentry: pending
  worker (depth 1): failed (0/1 ready)
  Config sentry: failed
────────────────────────────────────────
4 unchanged · 2 failed · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
depth 1
  ✔ web  unchanged
  ✘ worker  failed (0/1 ready)  0.0s
────────────────────────────────────────
4 unchanged · 2 failed · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
  "admitted_at": 1792529778,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529791,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 20
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "7ef95d0f-2897-4cef-9552-8ca17ad458de",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 11",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792529789,
          "history": [
            {
              "at": 1792529788,
              "state": "pending"
            },
            {
              "at": 1792529789,
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
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792529789,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792529789,
          "history": null,
          "log": null,
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792529789,
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
      "unexecuted": 0
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
        "api",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "ployz-create-a7ad025362f8c6d729bedcbd3a626a72891bacd1963b7897ec7a96d002dce971",
        "index": 0,
        "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "old_container_id": "e7d007f09e9e4fa4d48d266b94459a095b4ab8d14dd64777ed6e68658e872dd3",
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
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b"
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
            "service_id": "ac3479473030440d8119a14a083c9707",
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
  "runner": "cloud-01M5DHKHWAY2ZGTS4JYB9NB8G3",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 7,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": 1792529785,
  "status": "failed",
  "upload": null,
  "warnings": [
    "Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.",
    "Left PORT empty in sentry/config.yml: it is a broken reference."
  ]
}
```

stderr:
```
Deploying #11 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): waiting 0/1
  Volume pg-data: unchanged
  Config sentry: pending
  worker (depth 1): failed (0/1 ready)
  Config sentry: failed
────────────────────────────────────────
4 unchanged · 2 failed · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
error: worker: pull image failed.
  cause: Docker responded with status code 500: error from registry: denied; denied
inspect: ployz get worker.image --project shop --env production
undo: ployz history undo 7 --project shop --env production
```

### deployment show of the failed Deployment

```console
$ ployz deployment show 7ef95d0f-2897-4cef-9552-8ca17ad458de
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
  web: unchanged
  worker: failed
  pg-data: unchanged
  sentry: failed
```

### deployment show --json of the failed Deployment

```console
$ ployz --json deployment show 7ef95d0f-2897-4cef-9552-8ca17ad458de
# exit 0
```

stdout:
```
{
  "admitted_at": 1792529778,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529791,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 20
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "7ef95d0f-2897-4cef-9552-8ca17ad458de",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792529789,
          "history": [
            {
              "at": 1792529788,
              "state": "pending"
            },
            {
              "at": 1792529789,
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
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792529789,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792529789,
          "history": null,
          "log": null,
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792529789,
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
      "unexecuted": 0
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
        "api",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "ployz-create-a7ad025362f8c6d729bedcbd3a626a72891bacd1963b7897ec7a96d002dce971",
        "index": 0,
        "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "old_container_id": "e7d007f09e9e4fa4d48d266b94459a095b4ab8d14dd64777ed6e68658e872dd3",
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
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b"
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
            "service_id": "ac3479473030440d8119a14a083c9707",
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
  "runner": "cloud-01M5DHKHWAY2ZGTS4JYB9NB8G3",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 7,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": 1792529785,
  "status": "failed",
  "upload": null,
  "warnings": [
    {
      "broken": true,
      "config": "sentry",
      "file": "config.yml",
      "secret": "PLOYZ_PRIVATE_DOMAIN"
    },
    {
      "broken": true,
      "config": "sentry",
      "file": "config.yml",
      "secret": "PORT"
    }
  ]
}
```

### deployment redeploy of the failed Deployment

```console
$ ployz deployment redeploy 7ef95d0f-2897-4cef-9552-8ca17ad458de
# exit 1
```

stderr:
```
Deploying #12 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): waiting 0/1
  Volume pg-data: unchanged
  Config sentry: pending
  worker (depth 1): failed (0/1 ready)
  Config sentry: failed
────────────────────────────────────────
4 unchanged · 2 failed · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
error: worker: pull image failed.
  cause: Docker responded with status code 500: error from registry: denied; denied
inspect: ployz get worker.image --project shop --env production
undo: ployz history undo 7 --project shop --env production
```

### deployment redeploy --json of the failed Deployment

```console
$ ployz --json deployment redeploy f66ba7f2-9adc-4341-8130-3f98a070f663
# exit 1
```

stdout:
```
{
  "admitted_at": 1792529809,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529821,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 20
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "38e121b1-7325-4fab-a46a-c63ec8d5ead4",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 13",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792529819,
          "history": [
            {
              "at": 1792529818,
              "state": "pending"
            },
            {
              "at": 1792529819,
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
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792529819,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792529819,
          "history": null,
          "log": null,
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792529819,
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
      "unexecuted": 0
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
        "api",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
      "web": 1,
      "worker": 1
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "ployz-create-a7ad025362f8c6d729bedcbd3a626a72891bacd1963b7897ec7a96d002dce971",
        "index": 0,
        "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "97d7ad4b93fc402d9b5433d23f9e9fef",
          "old_container_id": "e7d007f09e9e4fa4d48d266b94459a095b4ab8d14dd64777ed6e68658e872dd3",
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
              "image": "ghcr.io/getployz/does-not-exist:nope",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b"
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
            "service_id": "ac3479473030440d8119a14a083c9707",
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
  "runner": "cloud-01M5DHMFR21RK8VMZRV2842ZDR",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 7,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": 1792529815,
  "status": "failed",
  "upload": null,
  "warnings": [
    "Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.",
    "Left PORT empty in sentry/config.yml: it is a broken reference."
  ]
}
```

stderr:
```
Deploying #13 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): waiting 0/1
  Volume pg-data: unchanged
  Config sentry: pending
  worker (depth 1): failed (0/1 ready)
  Config sentry: failed
────────────────────────────────────────
4 unchanged · 2 failed · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
$ ployz deployment cancel 7fad41a8-fa23-4b64-87ca-23eea4f13196
# exit 0
```

stdout:
```
Deployment #17 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment cancel --json a queued Deployment

```console
$ ployz --json deployment cancel 9a9799b0-ffea-4986-9015-e2b47ee8a7f2
# exit 0
```

stdout:
```
{
  "admitted_at": 1792529838,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529838,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 23
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "9a9799b0-ffea-4986-9015-e2b47ee8a7f2",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 19",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "not_attempted",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
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
    "web": "web",
    "worker": "worker"
  },
  "saved": 10,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": null,
  "status": "cancelled",
  "upload": null
}
```

### deployment start a queued Deployment

```console
$ ployz deployment start ac4d216e-6986-4d57-ad6d-9d4d75abd44b
# exit 0
```

stderr:
```
Deploying #21 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
  web (depth 1): booting 0/1
! web's image nginx:1.27-alpine exposes 80; its domains send to 8080. Set the domain port or PORT.
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 5 unchanged · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### deployment start --json a queued Deployment

```console
$ ployz --json deployment start eca7aaa4-89e0-4076-b508-6698b6e3d737
# exit 0
```

stdout:
```
{
  "admitted_at": 1792529880,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792529896,
  "environment": {
    "id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
    "name": "production",
    "project": "shop",
    "revision": 23
  },
  "environment_id": "038d47d9-f28a-4d2a-bf12-1c2cec06e488",
  "held": false,
  "id": "eca7aaa4-89e0-4076-b508-6698b6e3d737",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 23",
  "nodes": [
    {
      "id": "18a96a82-7c6a-4837-9671-36d65236b464",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "6ed6ed8f-aae7-4380-b69d-bb11cfd4aae9",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "290cf95f-21a3-4e8b-96e6-8bf4e5bec636",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a6c7e52-cfe4-4879-aafd-a4355c3d519b",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "a7444939-950d-4b3d-8300-5dafbd10171f",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "c051d44e-bb3b-4d02-8f92-5a907e8f264a",
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
        "api",
        "worker"
      ]
    ],
    "depths": {
      "api": 0,
      "postgres": 0,
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
  "runner": "cloud-01M5DHPNV2TBZ08H3JEZCYB7KK",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 10,
  "services": [],
  "ships": [
    "api",
    "web",
    "worker",
    "postgres"
  ],
  "started_at": 1792529891,
  "status": "applied",
  "upload": null,
  "warnings": [
    "Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.",
    "Left PORT empty in sentry/config.yml: it is a broken reference."
  ]
}
```

stderr:
```
Nothing changed on the Servers; Deployment #23 shipped version #10.
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### deployment cancel a running Deployment

```console
$ ployz deployment cancel 879f7810-61e2-468d-b060-a24be9185c56
# exit 0
```

stdout:
```
Deployment #24 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment show of a cancelled Deployment

```console
$ ployz deployment show 879f7810-61e2-468d-b060-a24be9185c56
# exit 0
```

stdout:
```
Deployment #24 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment redeploy of a cancelled Deployment

```console
$ ployz deployment redeploy 879f7810-61e2-468d-b060-a24be9185c56
# exit 0
```

stderr:
```
Deploying #25 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
  web (depth 1): booting 0/1
! web's image nginx:1.29-alpine exposes 80; its domains send to 8080. Set the domain port or PORT.
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 5 unchanged · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
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
Dashboard: http://localhost:46289/cloud/ada/workspace/production
```

stderr:
```
Created Project workspace.
Added Service workspace.
Deploying #1 of workspace/production
  workspace: waiting (building)
  workspace: waiting (building on machine-1)
  still waiting: workspace (waiting)
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
Dashboard: http://localhost:46289/cloud/ada/workspace/production
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
  "dashboard": "http://localhost:46289/cloud/ada/workspace/production",
  "deployment": {
    "admitted_at": 1792530441,
    "admitted_by": "Ada Lovelace",
    "builds": [
      {
        "commit": null,
        "finished_at": 1792530450,
        "github_skipped": null,
        "message": null,
        "server": null,
        "service": "workspace",
        "started_at": 1792530449,
        "status": "reused"
      }
    ],
    "ci": null,
    "ended_at": 1792530454,
    "environment": {
      "id": "2b88b32b-9b44-43c8-a57b-6b9baa984e9d",
      "name": "production",
      "project": "workspace",
      "revision": 3
    },
    "environment_id": "2b88b32b-9b44-43c8-a57b-6b9baa984e9d",
    "held": false,
    "id": "6d3a6092-ee4b-4ed6-88ea-ea5b8df17c9f",
    "in_flight": false,
    "message": null,
    "namespace": "workspace-production",
    "nodes": [
      {
        "id": "ef71dfd0-5af6-41f6-895d-ea4d4f91deef",
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
    "runner": "cloud-01M5DJ7SQ4DWDG5YT7A0XZMD8Y",
    "runtime_names": {
      "workspace": "workspace"
    },
    "saved": 1,
    "services": [],
    "ships": [
      "workspace"
    ],
    "started_at": 1792530448,
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
Deploying #3 of workspace/production
  workspace: waiting (building)
  workspace: waiting (build reused)
  workspace (depth 0): unchanged (build reused)
Nothing changed on the Servers; Deployment #3 shipped version #1.
```

### status after up (two Projects)

```console
$ ployz status
# exit 0
```

stdout:
```
cloud = http://localhost:46289
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
Approve 6e8ccc6f-6b06-4f74-9a49-618d71f43b49 under Waiting on you in the Ployz agent at http://localhost:46289/cloud/ada?chat=cae2df82-f53a-48e8-aa75-3aebbf548838
next: ployz org approval approve 6e8ccc6f-6b06-4f74-9a49-618d71f43b49
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
cloud = http://localhost:46289
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
    "cloud": "http://localhost:46289",
    "credential": "token",
    "organization": {
      "id": "71cdc2f0-4d57-4ff2-a9fe-42212732cd73",
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

