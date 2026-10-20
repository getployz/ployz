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
next: ployz deploy --expect-version 63:38:0.71
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
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 63
  },
  "ingress": [],
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 63:38:0.71",
  "unresolved": [
    "operations"
  ],
  "version": "63:38:0.71"
}
```

### deploy, nothing staged

```console
$ ployz deploy
# exit 0
```

stderr:
```
Nothing changed on the Servers; Deployment #87 shipped version #38.
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
  "admitted_at": 1792518259,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518271,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 63
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "e654dc52-37cf-4a4a-8f9d-ff7e270aca50",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 88",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 88,
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
  "runner": "cloud-01M5D6M0TDZD564BWXZK2TEDKA",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 38,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": 1792518266,
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
Nothing changed on the Servers; Deployment #88 shipped version #38.
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
next: ployz deploy --expect-version 64:38:0.73
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
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "lifecycle": "update",
      "name": "web",
      "restarts": [],
      "row": "e358ac6a-560a-468f-9f9f-43fc58547dfd:node",
      "settings": [
        {
          "after": "hello",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "web.env.GREETING",
          "row": "e358ac6a-560a-468f-9f9f-43fc58547dfd:variables.GREETING"
        }
      ],
      "type": "service"
    }
  ],
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 64
  },
  "ingress": [],
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 64:38:0.73",
  "unresolved": [
    "operations"
  ],
  "version": "64:38:0.73"
}
```

### deploy a staged change (live follower) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Deploying #89 of shop/production
✔ Volume pg-data  unchanged
✔ Config sentry  unchanged
depth 0
  ✔ api  unchanged
  ✔ postgres  unchanged
depth 1
  ✔ web  switched 1/1  14.0s
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
Deploying #90 of shop/production
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
  "admitted_at": 1792518354,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518382,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 66
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "8983cfb2-ac2e-4fd0-8fcc-adc1498c69bb",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 91",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792518380,
          "history": [
            {
              "at": 1792518366,
              "state": "pending"
            },
            {
              "at": 1792518367,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792518367,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792518373,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792518380,
              "state": "completed"
            }
          ],
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "server": "machine-1",
          "started_at": 1792518367,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [
        {
          "finished_at": 1792518380,
          "history": null,
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "server": "machine-1",
          "started_at": 1792518367,
          "state": "completed"
        }
      ],
      "type": "config"
    }
  ],
  "number": 91,
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
        "display_name": "ployz-create-b1dc3f52083f569537afbd30056999445cdea8d2abe346e05fa8f83f84d8b9da",
        "index": 0,
        "machine_id": "c109917d492d460a994dbf4b43162146",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "old_container_id": "19f398006691f842d9b2f3ab85b4e426d0a2d12b2c90c1cd3c903d74b92a96df",
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
                "cloud.ployz.service.id": "e358ac6a-560a-468f-9f9f-43fc58547dfd"
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
            "service_id": "0bc3883482ba480298f83b694d408c4d",
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
  "runner": "cloud-01M5D6PXZDQ0BQC4HD70Y3XGA7",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 41,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": 1792518361,
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
Deploying #91 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web's image nginx:1.27-alpine exposes 80; its domains send to 8080. Set the domain port or PORT.
  web (depth 1): booting 0/1
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
Deployment #92 of shop/production: queued
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
  "admitted_at": 1792518418,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": null,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 68
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "a50825a1-5a3c-4faa-ae8c-174e74ff152a",
  "in_flight": true,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 93",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "pending",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "pending",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "pending",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 93,
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
  "saved": 43,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
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
#93	applied	43
#92	applied	42
#91	applied	41
#90	applied	40
#89	applied	39
```

stderr:
```
next: ployz deployment ls --cursor ODk --limit 5
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
      "admitted_at": 1792518418,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": 1792518445,
      "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
      "held": false,
      "id": "a50825a1-5a3c-4faa-ae8c-174e74ff152a",
      "in_flight": false,
      "message": null,
      "number": 93,
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
      "runner": "cloud-01M5D6RVR9D8YYQHFD5VJXTDPR",
      "saved": 43,
      "services": [],
      "ships": [
        "api",
        "postgres",
        "web",
        "worker"
      ],
      "started_at": 1792518425,
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
              "c109917d492d460a994dbf4b43162146"
            ],
            "routed": [
              8080
            ],
            "service": "web",
            "service_id": "0bc3883482ba480298f83b694d408c4d",
            "type": "image_port_mismatch"
          }
        }
      ]
    },
    {
      "admitted_at": 1792518386,
      "admitted_by": "Ada Lovelace",
      "ci": null,
      "ended_at": 1792518412,
      "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
      "held": false,
      "id": "0d24bb11-31f5-4ba8-a8a6-637e03674529",
      "in_flight": false,
      "message": null,
      "number": 92,
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
      "runner": "cloud-01M5D6QWKX9WRPJFZAAYMYW5EG",
      "saved": 42,
      "services": [],
      "ships": [
        "api",
        "postgres",
        "web",
        "worker"
      ],
      "started_at": 1792518393,
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
              "c109917d492d460a994dbf4b43162146"
            ],
            "routed": [
              8080
            ],
            "service": "web",
            "service_id": "0bc3883482ba480298f83b694d408c4d",
            "type": "image_port_mismatch"
          }
        }
      ]
    }
  ],
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 68
  },
  "in_flight": [],
  "latest": {
    "admitted_at": 1792518418,
    "admitted_by": "Ada Lovelace",
    "ci": null,
    "ended_at": 1792518445,
    "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "held": false,
    "id": "a50825a1-5a3c-4faa-ae8c-174e74ff152a",
    "in_flight": false,
    "message": null,
    "number": 93,
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
    "runner": "cloud-01M5D6RVR9D8YYQHFD5VJXTDPR",
    "saved": 43,
    "services": [],
    "ships": [
      "api",
      "postgres",
      "web",
      "worker"
    ],
    "started_at": 1792518425,
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
            "c109917d492d460a994dbf4b43162146"
          ],
          "routed": [
            8080
          ],
          "service": "web",
          "service_id": "0bc3883482ba480298f83b694d408c4d",
          "type": "image_port_mismatch"
        }
      }
    ]
  },
  "next": "ployz deployment ls --cursor OTI --limit 2",
  "next_cursor": "OTI"
}
```

### deployment show by id

```console
$ ployz deployment show a50825a1-5a3c-4faa-ae8c-174e74ff152a
# exit 0
```

stdout:
```
Deployment #93 of shop/production: applied
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
$ ployz deployment show 93
# exit 0
```

stdout:
```
Deployment #93 of shop/production: applied
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
$ ployz --json deployment show a50825a1-5a3c-4faa-ae8c-174e74ff152a
# exit 0
```

stdout:
```
{
  "admitted_at": 1792518418,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518445,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 68
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "a50825a1-5a3c-4faa-ae8c-174e74ff152a",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792518443,
          "history": [
            {
              "at": 1792518429,
              "state": "pending"
            },
            {
              "at": 1792518430,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792518430,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792518436,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792518443,
              "state": "completed"
            }
          ],
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "server": "machine-1",
          "started_at": 1792518430,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [
        {
          "finished_at": 1792518443,
          "history": null,
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "server": "machine-1",
          "started_at": 1792518430,
          "state": "completed"
        }
      ],
      "type": "config"
    }
  ],
  "number": 93,
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
        "display_name": "ployz-create-fe455cc548c3840d5129035d1e8144d49ccaf3974a1ed2487a742c26d0059337",
        "index": 0,
        "machine_id": "c109917d492d460a994dbf4b43162146",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "old_container_id": "e8282ddb21ff0aba6cf0dc75c8612e8ea3948c9bb441beed7a2a3f1d7fbf0817",
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
                "cloud.ployz.service.id": "e358ac6a-560a-468f-9f9f-43fc58547dfd"
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
            "service_id": "0bc3883482ba480298f83b694d408c4d",
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
  "runner": "cloud-01M5D6RVR9D8YYQHFD5VJXTDPR",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 43,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": 1792518425,
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
          "c109917d492d460a994dbf4b43162146"
        ],
        "routed": [
          8080
        ],
        "service": "web",
        "service_id": "0bc3883482ba480298f83b694d408c4d",
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
Deploying #94 of shop/production
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
undo: ployz history undo 44 --project shop --env production
```

### deploy that fails (live follower) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Deploying #95 of shop/production
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
undo: ployz history undo 44 --project shop --env production
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
  "admitted_at": 1792518489,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518502,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 69
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "dbe19366-0aae-4f7b-8cdd-33c4d3a22407",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 96",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792518500,
          "history": [
            {
              "at": 1792518499,
              "state": "pending"
            },
            {
              "at": 1792518500,
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
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792518500,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792518500,
          "history": null,
          "log": null,
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792518500,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "config"
    }
  ],
  "number": 96,
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
        "display_name": "ployz-create-af57ae4c8ee193deb9af715b3ade4e060cb09821ea5c48dfe127555471401bf9",
        "index": 0,
        "machine_id": "c109917d492d460a994dbf4b43162146",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "old_container_id": "c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d",
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
                "cloud.ployz.service.id": "eb6735e8-bf91-4e42-9348-e465eaea151e"
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
            "service_id": "b3b19a0846b2400d8920ccdb576dc8d0",
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
  "runner": "cloud-01M5D6V1CQSGTRFXPP4KVWM4EA",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 44,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": 1792518496,
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
Deploying #96 of shop/production
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
undo: ployz history undo 44 --project shop --env production
```

### deployment show of the failed Deployment

```console
$ ployz deployment show dbe19366-0aae-4f7b-8cdd-33c4d3a22407
# exit 0
```

stdout:
```
Deployment #96 of shop/production: failed
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
$ ployz --json deployment show dbe19366-0aae-4f7b-8cdd-33c4d3a22407
# exit 0
```

stdout:
```
{
  "admitted_at": 1792518489,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518502,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 69
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "dbe19366-0aae-4f7b-8cdd-33c4d3a22407",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792518500,
          "history": [
            {
              "at": 1792518499,
              "state": "pending"
            },
            {
              "at": 1792518500,
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
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792518500,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792518500,
          "history": null,
          "log": null,
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792518500,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "config"
    }
  ],
  "number": 96,
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
        "display_name": "ployz-create-af57ae4c8ee193deb9af715b3ade4e060cb09821ea5c48dfe127555471401bf9",
        "index": 0,
        "machine_id": "c109917d492d460a994dbf4b43162146",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "old_container_id": "c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d",
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
                "cloud.ployz.service.id": "eb6735e8-bf91-4e42-9348-e465eaea151e"
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
            "service_id": "b3b19a0846b2400d8920ccdb576dc8d0",
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
  "runner": "cloud-01M5D6V1CQSGTRFXPP4KVWM4EA",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 44,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": 1792518496,
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
$ ployz deployment redeploy dbe19366-0aae-4f7b-8cdd-33c4d3a22407
# exit 1
```

stderr:
```
Deploying #97 of shop/production
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
undo: ployz history undo 44 --project shop --env production
```

### deployment redeploy --json of the failed Deployment

```console
$ ployz --json deployment redeploy 60a4ba6a-23aa-44b5-9483-aea3057c2794
# exit 1
```

stdout:
```
{
  "admitted_at": 1792518521,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518534,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 69
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "3b96148c-14c5-4c7c-8811-6af243f15c4b",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 98",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792518532,
          "history": [
            {
              "at": 1792518531,
              "state": "pending"
            },
            {
              "at": 1792518532,
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
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792518532,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "failed",
      "rows": [
        {
          "cause": [
            "image pull failed",
            "Docker responded with status code 500: error from registry: denied\ndenied"
          ],
          "finished_at": 1792518532,
          "history": null,
          "log": null,
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "reason": "pull image failed",
          "server": "machine-1",
          "started_at": 1792518532,
          "state": "failed",
          "step": "pull"
        }
      ],
      "type": "config"
    }
  ],
  "number": 98,
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
        "display_name": "ployz-create-af57ae4c8ee193deb9af715b3ade4e060cb09821ea5c48dfe127555471401bf9",
        "index": 0,
        "machine_id": "c109917d492d460a994dbf4b43162146",
        "machine_name": "machine-1",
        "operation": {
          "machine_id": "c109917d492d460a994dbf4b43162146",
          "old_container_id": "c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d",
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
                "cloud.ployz.service.id": "eb6735e8-bf91-4e42-9348-e465eaea151e"
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
            "service_id": "b3b19a0846b2400d8920ccdb576dc8d0",
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
  "runner": "cloud-01M5D6W0GYVAYBMK7FA487KAKB",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 44,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": 1792518528,
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
Deploying #98 of shop/production
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
undo: ployz history undo 44 --project shop --env production
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
Nothing changed on the Servers; Deployment #99 shipped version #45.
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
#102	queued	47
#101	superseded	47
#100	superseded	46
#99	applied	45
```

stderr:
```
next: ployz deployment ls --cursor OTk --limit 4
```

### deployment cancel a queued Deployment

```console
$ ployz deployment cancel 1dfcdfd8-2f0b-479e-8e00-39ef55be262b
# exit 0
```

stdout:
```
Deployment #102 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment cancel --json a queued Deployment

```console
$ ployz --json deployment cancel 00442e73-3334-44e2-b4f9-96e7121f6f78
# exit 0
```

stdout:
```
{
  "admitted_at": 1792518558,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518559,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 72
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "00442e73-3334-44e2-b4f9-96e7121f6f78",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 104",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "not_attempted",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "not_attempted",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "not_attempted",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 104,
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
  "saved": 47,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": null,
  "status": "cancelled",
  "upload": null
}
```

### deployment start a queued Deployment

```console
$ ployz deployment start f4a075fe-2bb6-4a7e-9bec-6275bef21089
# exit 0
```

stderr:
```
Deploying #106 of shop/production
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
$ ployz --json deployment start be359db8-d829-431a-a6ed-3d1f70094444
# exit 0
```

stdout:
```
{
  "admitted_at": 1792518605,
  "admitted_by": "Ada Lovelace",
  "builds": [],
  "ci": null,
  "ended_at": 1792518621,
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 72
  },
  "environment_id": "e3ff9fdc-d394-4a02-9195-f717984132db",
  "held": false,
  "id": "be359db8-d829-431a-a6ed-3d1f70094444",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 108",
  "nodes": [
    {
      "id": "254b1264-6d96-4e34-86d2-795920ddab60",
      "name": "api",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "796ce242-5b1d-451d-8804-42de83e14920",
      "name": "postgres",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
      "name": "worker",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    },
    {
      "id": "2a748c8b-32bc-433a-be41-3420710e6b7c",
      "name": "pg-data",
      "outcome": "unchanged",
      "rows": [],
      "type": "volume"
    },
    {
      "id": "a18f32f9-878e-44e3-9e5d-57af1305969c",
      "name": "sentry",
      "outcome": "unchanged",
      "rows": [],
      "type": "config"
    }
  ],
  "number": 108,
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
  "runner": "cloud-01M5D6YK3V39KTSTSWE2R4AV6R",
  "runtime_names": {
    "api": "api",
    "postgres": "postgres",
    "web": "web",
    "worker": "worker"
  },
  "saved": 47,
  "services": [],
  "ships": [
    "api",
    "postgres",
    "web",
    "worker"
  ],
  "started_at": 1792518615,
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
Nothing changed on the Servers; Deployment #108 shipped version #47.
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### deployment cancel a running Deployment

```console
$ ployz deployment cancel 7036cf42-c1d8-48c6-ba0b-a2273b9c9a26
# exit 0
```

stdout:
```
Deployment #109 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment show of a cancelled Deployment

```console
$ ployz deployment show 7036cf42-c1d8-48c6-ba0b-a2273b9c9a26
# exit 0
```

stdout:
```
Deployment #109 of shop/production: cancelled
  api: not attempted
  postgres: not attempted
  web: not attempted
  worker: not attempted
  pg-data: not attempted
  sentry: not attempted
```

### deployment redeploy of a cancelled Deployment

```console
$ ployz deployment redeploy 7036cf42-c1d8-48c6-ba0b-a2273b9c9a26
# exit 0
```

stderr:
```
Deploying #110 of shop/production
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
Dashboard: http://localhost:38265/cloud/ada/workspace/production
```

stderr:
```
Created Project workspace.
Added Service workspace.
Deploying #1 of workspace/production
  workspace: waiting (building)
  still waiting: workspace (waiting)
  still waiting: workspace (waiting)
  workspace: waiting (built)
  workspace (depth 0): waiting 0/1
! workspace.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  workspace (depth 0): booting 0/1
  workspace (depth 0): switching 1/1
  workspace (depth 0): switched 1/1
────────────────────────────
1 deployed · across 1 server
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
Open https://workspace.ada.ployz.test
Dashboard: http://localhost:38265/cloud/ada/workspace/production
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
  "dashboard": "http://localhost:38265/cloud/ada/workspace/production",
  "deployment": {
    "admitted_at": 1792518858,
    "admitted_by": "Ada Lovelace",
    "builds": [
      {
        "commit": null,
        "finished_at": 1792518867,
        "message": null,
        "service": "workspace",
        "started_at": 1792518866,
        "status": "reused"
      }
    ],
    "ci": null,
    "ended_at": 1792518872,
    "environment": {
      "id": "1aaa8244-f347-41cc-bb0c-a6ad33e181e9",
      "name": "production",
      "project": "workspace",
      "revision": 3
    },
    "environment_id": "1aaa8244-f347-41cc-bb0c-a6ad33e181e9",
    "held": false,
    "id": "9ee50627-f100-403a-b21f-dae9006e9d78",
    "in_flight": false,
    "message": null,
    "namespace": "workspace-production",
    "nodes": [
      {
        "id": "72b9221f-13ce-42b4-97d4-80cde100ea67",
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
    "runner": "cloud-01M5D769JG4WV2N7066FN0HVJV",
    "runtime_names": {
      "workspace": "workspace"
    },
    "saved": 1,
    "services": [],
    "ships": [
      "workspace"
    ],
    "started_at": 1792518865,
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
cloud = http://localhost:38265
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

### project rm the up Project

```console
$ ployz project rm workspace --confirm workspace
# exit 124
```

stderr:
```
Removing workspace removes Service workspace.
This removal in Organization ada destroys 1 thing:
  ✕ removes Service workspace
Approve 68f6df1e-b2b4-4a0a-a907-6553bdaf2e8d under Waiting on you in the Ployz agent at http://localhost:38265/cloud/ada?chat=b8ce4064-ce85-43d2-abb4-8f7e78911842
next: ployz org approval approve 68f6df1e-b2b4-4a0a-a907-6553bdaf2e8d
Waiting for approval in Ployz Cloud… 
Timed out after 300s
```

### status with the directory still linked to the removed Project

```console
$ ployz status
# exit 0
```

stdout:
```
cloud = http://localhost:38265
organization = ada
signed in as = token
environment = workspace/production
linked from = <run-dir>/workspace
staged = 0 changes
```

### status --json with the directory still linked to the removed Project

```console
$ ployz --json status
# exit 0
```

stdout:
```
{
  "attention": [],
  "deploying": [],
  "environment": {
    "id": "1aaa8244-f347-41cc-bb0c-a6ad33e181e9",
    "name": "production",
    "project": "workspace",
    "revision": 3
  },
  "identity": {
    "cloud": "http://localhost:38265",
    "credential": "token",
    "organization": {
      "id": "c5bb4022-ae5d-439a-b670-d567afb5f40a",
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
  "staged": {
    "changes": 0,
    "saved": true,
    "version": "3:1:0.3"
  }
}
```

