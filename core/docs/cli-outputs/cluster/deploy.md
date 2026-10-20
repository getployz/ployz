### project new

```console
$ ployz project new shop
# exit 0
```

stdout:
```
Created Project shop with Environment production.
```

### service add web

```console
$ ployz service add web --image alpine:3.23.3
# exit 0
```

stdout:
```
Staged new Service web in shop/production.
```

stderr:
```
next: ployz deploy
```

### set web.startCommand

```console
$ ployz set web.startCommand=sh\ -c\ \'echo\ started\;\ while\ true\;\ do\ echo\ tick\;\ sleep\ 2\;\ done\'
# exit 0
```

stdout:
```
Staged web.startCommand in shop/production.
```

stderr:
```
next: ployz deploy
```

### set web.replicas

```console
$ ployz set web.replicas=2
# exit 0
```

stdout:
```
Staged web.replicas in shop/production.
```

stderr:
```
next: ployz deploy
```

### service add api

```console
$ ployz service add api --image alpine:3.23.3
# exit 0
```

stdout:
```
Staged new Service api in shop/production.
```

stderr:
```
next: ployz deploy
```

### set api.startCommand

```console
$ ployz set api.startCommand=sh\ -c\ \'echo\ api\ up\;\ sleep\ 100000\'
# exit 0
```

stdout:
```
Staged api.startCommand in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy --plan

```console
$ ployz deploy --plan
# exit 0
```

stdout:
```
api (create)
  api.image: null → "alpine:3.23.3"
  api.startCommand: null → "sh -c 'echo api up; sleep 100000'"
web (create)
  web.image: null → "alpine:3.23.3"
  web.startCommand: null → "sh -c 'echo started; while true; do echo tick; sleep 2; done'"
  web.replicas: 1 → 2
Decided by the Servers when it runs: which containers start and stop.
```

stderr:
```
next: ployz deploy --expect-version 6:0:0.0
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
      "comparison": "introduction",
      "data": null,
      "id": "e0eccf29-1258-4c57-829f-2250eec054c4",
      "lifecycle": "create",
      "name": "api",
      "restarts": [],
      "row": "e0eccf29-1258-4c57-829f-2250eec054c4:node",
      "settings": [
        {
          "after": "alpine:3.23.3",
          "before": null,
          "canRestore": false,
          "kind": "add",
          "path": "api.image",
          "row": "e0eccf29-1258-4c57-829f-2250eec054c4:source"
        },
        {
          "after": "sh -c 'echo api up; sleep 100000'",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "api.startCommand",
          "row": "e0eccf29-1258-4c57-829f-2250eec054c4:startCommand"
        }
      ],
      "type": "service"
    },
    {
      "comparison": "introduction",
      "data": null,
      "id": "608e4644-cd97-46d2-bb82-416f75554478",
      "lifecycle": "create",
      "name": "web",
      "restarts": [],
      "row": "608e4644-cd97-46d2-bb82-416f75554478:node",
      "settings": [
        {
          "after": "alpine:3.23.3",
          "before": null,
          "canRestore": false,
          "kind": "add",
          "path": "web.image",
          "row": "608e4644-cd97-46d2-bb82-416f75554478:source"
        },
        {
          "after": "sh -c 'echo started; while true; do echo tick; sleep 2; done'",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "web.startCommand",
          "row": "608e4644-cd97-46d2-bb82-416f75554478:startCommand"
        },
        {
          "after": 2,
          "before": 1,
          "canRestore": true,
          "kind": "update",
          "path": "web.replicas",
          "row": "608e4644-cd97-46d2-bb82-416f75554478:replicas"
        }
      ],
      "type": "service"
    }
  ],
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 6
  },
  "ingress": [],
  "namespace": "shop-production",
  "next": "ployz deploy --expect-version 6:0:0.0",
  "unresolved": [
    "operations"
  ],
  "version": "6:0:0.0"
}
```

### deploy (pipe, first deploy)

```console
$ ployz deploy --message first\ deploy
# exit 0
```

stderr:
```
Deploying #1 of shop/production
  api (depth 0): booting 0/1
  web (depth 0): booting 0/2
  api (depth 0): switching 1/1
  web (depth 0): switching 2/2
  api (depth 0): switched 1/1
  web (depth 0): switched 2/2
─────────────────────────────
2 deployed · across 3 servers
```

### set web.env (staged change)

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

### deploy (live progress) (TTY)

```console
$ ployz deploy --message greeting
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Deploying #2 of shop/production
depth 0
  ✔ api  unchanged
  ✔ web  switched 2/2  26.0s
───────────────────────────────────────────
1 deployed · 1 unchanged · across 2 servers
# exit 0
```

### set api.env (staged change)

```console
$ ployz set api.env.MODE=fast
# exit 0
```

stdout:
```
Staged api.env.MODE in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy --json --events (result + NDJSON progress)

```console
$ ployz deploy --json --events /tmp/ployz-cap-home/events.ndjson
# exit 0
```

stdout:
```
{
  "admitted_at": 1792513033,
  "admitted_by": null,
  "builds": [],
  "ci": null,
  "ended_at": 1792513064,
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 8
  },
  "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
  "held": false,
  "id": "4083f374-2bae-415f-a3ab-40eb9bc5f01e",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show 3",
  "nodes": [
    {
      "id": "e0eccf29-1258-4c57-829f-2250eec054c4",
      "name": "api",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792513064,
          "history": [
            {
              "at": 1792513034,
              "state": "pending"
            },
            {
              "at": 1792513034,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513034,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513041,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513064,
              "state": "completed"
            }
          ],
          "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
          "server": "beta",
          "started_at": 1792513034,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "608e4644-cd97-46d2-bb82-416f75554478",
      "name": "web",
      "outcome": "unchanged",
      "rows": [],
      "type": "service"
    }
  ],
  "number": 3,
  "outcome": {
    "cause": [],
    "reason": null,
    "services": [
      {
        "result": "new",
        "service": "api"
      }
    ],
    "summary": {
      "completed": 1,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [],
    "depths": {
      "api": 0,
      "web": 0
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": "ployz-create-d4c8fba060a72cdcf18987e89532e15223c1c2bcb59fac8acbd5b2126a7d7c19",
        "index": 0,
        "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
        "machine_name": "beta",
        "operation": {
          "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
          "old_container_id": "daee0729557fb67c317f78e7f5da9cbb699325db1212275ba683ed37c2fe1e47",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [
                "sh",
                "-c",
                "echo api up; sleep 100000"
              ],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "state": "disabled"
              },
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "e0eccf29-1258-4c57-829f-2250eec054c4"
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
            "name": "api",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "ae4d79d2ba6b4c53b2297d5a40ef5c92",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "replace_container"
        },
        "service_name": "api",
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
  "runner": "cli-19c73fec-9be6-45a7-89eb-f4c6ea1bdf85",
  "runtime_names": {
    "api": "api",
    "web": "web"
  },
  "saved": 3,
  "services": [],
  "ships": [
    "web",
    "api"
  ],
  "started_at": 1792513033,
  "status": "applied",
  "upload": null
}
```

stderr:
```
Deploying #3 of shop/production
  api (depth 0): booting 0/1
  web (depth 0): unchanged
  api (depth 0): switching 1/1
  api (depth 0): switched 1/1
──────────────────────────────────────────
1 deployed · 1 unchanged · across 1 server
```

### deploy --events NDJSON (first 15 lines)

```
{"id":"4083f374-2bae-415f-a3ab-40eb9bc5f01e","nodes":[{"id":"e0eccf29-1258-4c57-829f-2250eec054c4","name":"api","outcome":"pending","rows":[],"type":"service"},{"id":"608e4644-cd97-46d2-bb82-416f75554478","name":"web","outcome":"pending","rows":[],"type":"service"}],"number":3,"status":"queued","type":"deployment"}
{"id":"4083f374-2bae-415f-a3ab-40eb9bc5f01e","nodes":[{"id":"e0eccf29-1258-4c57-829f-2250eec054c4","name":"api","outcome":"pending","rows":[{"finished_at":null,"history":[{"at":1792513034,"state":"pending"},{"at":1792513034,"phase":"starting","serving":false,"state":"running"},{"at":1792513034,"phase":"creating_container","serving":false,"state":"running"}],"machine_id":"7f00d39ce93641bd8c5fdd7d91395aa7","phase":"creating_container","server":"beta","serving":false,"started_at":1792513034,"state":"running"}],"type":"service"},{"id":"608e4644-cd97-46d2-bb82-416f75554478","name":"web","outcome":"unchanged","rows":[],"type":"service"}],"number":3,"status":"running","type":"deployment"}
{"id":"4083f374-2bae-415f-a3ab-40eb9bc5f01e","nodes":[{"id":"e0eccf29-1258-4c57-829f-2250eec054c4","name":"api","outcome":"pending","rows":[{"finished_at":null,"history":[{"at":1792513034,"state":"pending"},{"at":1792513034,"phase":"starting","serving":false,"state":"running"},{"at":1792513034,"phase":"creating_container","serving":false,"state":"running"},{"at":1792513041,"phase":"waiting_for_switch","serving":false,"state":"running"}],"machine_id":"7f00d39ce93641bd8c5fdd7d91395aa7","phase":"waiting_for_switch","server":"beta","serving":false,"started_at":1792513034,"state":"running"}],"type":"service"},{"id":"608e4644-cd97-46d2-bb82-416f75554478","name":"web","outcome":"unchanged","rows":[],"type":"service"}],"number":3,"status":"running","type":"deployment"}
{"id":"4083f374-2bae-415f-a3ab-40eb9bc5f01e","nodes":[{"id":"e0eccf29-1258-4c57-829f-2250eec054c4","name":"api","outcome":"deployed","rows":[{"finished_at":1792513064,"history":[{"at":1792513034,"state":"pending"},{"at":1792513034,"phase":"starting","serving":false,"state":"running"},{"at":1792513034,"phase":"creating_container","serving":false,"state":"running"},{"at":1792513041,"phase":"waiting_for_switch","serving":false,"state":"running"},{"at":1792513064,"state":"completed"}],"machine_id":"7f00d39ce93641bd8c5fdd7d91395aa7","server":"beta","started_at":1792513034,"state":"completed"}],"type":"service"},{"id":"608e4644-cd97-46d2-bb82-416f75554478","name":"web","outcome":"unchanged","rows":[],"type":"service"}],"number":3,"status":"applied","type":"deployment"}
```

### deploy with nothing staged

```console
$ ployz deploy
# exit 0
```

stderr:
```
Nothing changed on the Servers; Deployment #4 shipped version #3.
```

### deploy --detach

```console
$ ployz deploy --detach
# exit 2
```

stderr:
```
error: The hidden local Store runs Deployments in this process, so it can't detach
```

### deployment ls

```console
$ ployz deployment ls
# exit 0
```

stdout:
```
DEPLOYMENT	STATUS	SAVED REVISION
#5	applied	3
#4	applied	3
#3	applied	3
#2	applied	2
#1	applied	1
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
      "admitted_at": 1792513067,
      "admitted_by": null,
      "ci": null,
      "ended_at": 1792513067,
      "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
      "held": false,
      "id": "a2614f69-b3c6-4b26-998f-1cf73bb070a5",
      "in_flight": false,
      "message": null,
      "number": 5,
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
      "push": null,
      "remove": false,
      "runner": "cli-d9fa3ac9-098d-4702-b36b-c303a2e59ffb",
      "saved": 3,
      "services": [],
      "ships": [
        "web",
        "api"
      ],
      "started_at": 1792513067,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1792513065,
      "admitted_by": null,
      "ci": null,
      "ended_at": 1792513066,
      "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
      "held": false,
      "id": "e33b5719-1114-431c-95a3-9ec1f5308151",
      "in_flight": false,
      "message": null,
      "number": 4,
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
      "push": null,
      "remove": false,
      "runner": "cli-b0825bb0-4c68-487c-884a-ce2d4cf44363",
      "saved": 3,
      "services": [],
      "ships": [
        "web",
        "api"
      ],
      "started_at": 1792513065,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1792513033,
      "admitted_by": null,
      "ci": null,
      "ended_at": 1792513064,
      "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
      "held": false,
      "id": "4083f374-2bae-415f-a3ab-40eb9bc5f01e",
      "in_flight": false,
      "message": null,
      "number": 3,
      "outcome": {
        "cause": [],
        "reason": null,
        "services": [
          {
            "result": "new",
            "service": "api"
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
      "runner": "cli-19c73fec-9be6-45a7-89eb-f4c6ea1bdf85",
      "saved": 3,
      "services": [],
      "ships": [
        "web",
        "api"
      ],
      "started_at": 1792513033,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1792513005,
      "admitted_by": null,
      "ci": null,
      "ended_at": 1792513032,
      "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
      "held": false,
      "id": "3baf7c10-b3b4-4723-8bf0-d3cdf0012195",
      "in_flight": false,
      "message": "greeting",
      "number": 2,
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
      "runner": "cli-369b3933-6b3e-4ee1-b79d-0866153438a9",
      "saved": 2,
      "services": [],
      "ships": [
        "web",
        "api"
      ],
      "started_at": 1792513005,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1792512994,
      "admitted_by": null,
      "ci": null,
      "ended_at": 1792513004,
      "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
      "held": false,
      "id": "8040ba9e-75a4-4531-b2c2-1d0739001422",
      "in_flight": false,
      "message": "first deploy",
      "number": 1,
      "outcome": {
        "cause": [],
        "reason": null,
        "services": [
          {
            "result": "new",
            "service": "web"
          },
          {
            "result": "new",
            "service": "api"
          }
        ],
        "summary": {
          "completed": 3,
          "type": "success"
        },
        "type": "executed"
      },
      "push": null,
      "remove": false,
      "runner": "cli-6d855c52-6332-40f3-96ff-f122dd5705a5",
      "saved": 1,
      "services": [],
      "ships": [
        "web",
        "api"
      ],
      "started_at": 1792512994,
      "status": "applied",
      "upload": null
    }
  ],
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 8
  },
  "in_flight": [],
  "latest": {
    "admitted_at": 1792513067,
    "admitted_by": null,
    "ci": null,
    "ended_at": 1792513067,
    "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "held": false,
    "id": "a2614f69-b3c6-4b26-998f-1cf73bb070a5",
    "in_flight": false,
    "message": null,
    "number": 5,
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
    "push": null,
    "remove": false,
    "runner": "cli-d9fa3ac9-098d-4702-b36b-c303a2e59ffb",
    "saved": 3,
    "services": [],
    "ships": [
      "web",
      "api"
    ],
    "started_at": 1792513067,
    "status": "applied",
    "upload": null
  },
  "next_cursor": null
}
```

### deployment show

```console
$ ployz deployment show 8040ba9e-75a4-4531-b2c2-1d0739001422
# exit 0
```

stdout:
```
Deployment #1 of shop/production: applied
  web: serves its new copies
  api: serves its new copies
  api: deployed
  web: deployed
```

### deployment show --json

```console
$ ployz deployment show 8040ba9e-75a4-4531-b2c2-1d0739001422 --json
# exit 0
```

stdout:
```
{
  "admitted_at": 1792512994,
  "admitted_by": null,
  "builds": [],
  "ci": null,
  "ended_at": 1792513004,
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 8
  },
  "environment_id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
  "held": false,
  "id": "8040ba9e-75a4-4531-b2c2-1d0739001422",
  "in_flight": false,
  "message": "first deploy",
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "e0eccf29-1258-4c57-829f-2250eec054c4",
      "name": "api",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792513003,
          "history": [
            {
              "at": 1792512995,
              "state": "pending"
            },
            {
              "at": 1792512995,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792512995,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513001,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513003,
              "state": "completed"
            }
          ],
          "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
          "server": "beta",
          "started_at": 1792512995,
          "state": "completed"
        }
      ],
      "type": "service"
    },
    {
      "id": "608e4644-cd97-46d2-bb82-416f75554478",
      "name": "web",
      "outcome": "deployed",
      "rows": [
        {
          "finished_at": 1792513003,
          "history": [
            {
              "at": 1792512995,
              "state": "pending"
            },
            {
              "at": 1792512995,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792512995,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513001,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513003,
              "state": "completed"
            }
          ],
          "machine_id": "7f571356a2ff4c36abb2cd9a719ec8ec",
          "server": "gamma",
          "started_at": 1792512995,
          "state": "completed"
        },
        {
          "finished_at": 1792513003,
          "history": [
            {
              "at": 1792512995,
              "state": "pending"
            },
            {
              "at": 1792512995,
              "phase": "starting",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792512995,
              "phase": "creating_container",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513001,
              "phase": "waiting_for_switch",
              "serving": false,
              "state": "running"
            },
            {
              "at": 1792513003,
              "state": "completed"
            }
          ],
          "machine_id": "91e8bbd81cd34c59a186d49d3375bbe8",
          "server": "alpha",
          "started_at": 1792512995,
          "state": "completed"
        }
      ],
      "type": "service"
    }
  ],
  "number": 1,
  "outcome": {
    "cause": [],
    "reason": null,
    "services": [
      {
        "result": "new",
        "service": "web"
      },
      {
        "result": "new",
        "service": "api"
      }
    ],
    "summary": {
      "completed": 3,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "dependencies": [],
    "depths": {
      "api": 0,
      "web": 0
    },
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": null,
        "index": 0,
        "machine_id": "7f571356a2ff4c36abb2cd9a719ec8ec",
        "machine_name": "gamma",
        "operation": {
          "machine_id": "7f571356a2ff4c36abb2cd9a719ec8ec",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [
                "sh",
                "-c",
                "echo started; while true; do echo tick; sleep 2; done"
              ],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "state": "disabled"
              },
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "608e4644-cd97-46d2-bb82-416f75554478"
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
              "replicas": 2
            },
            "mounts": [],
            "name": "web",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "8df4db26a03241d2a935b04ab159deef",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "run_container"
        },
        "service_name": "web",
        "status": {
          "type": "pending"
        }
      },
      {
        "display_name": null,
        "index": 1,
        "machine_id": "91e8bbd81cd34c59a186d49d3375bbe8",
        "machine_name": "alpha",
        "operation": {
          "machine_id": "91e8bbd81cd34c59a186d49d3375bbe8",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [
                "sh",
                "-c",
                "echo started; while true; do echo tick; sleep 2; done"
              ],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "state": "disabled"
              },
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "608e4644-cd97-46d2-bb82-416f75554478"
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
              "replicas": 2
            },
            "mounts": [],
            "name": "web",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "8df4db26a03241d2a935b04ab159deef",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "run_container"
        },
        "service_name": "web",
        "status": {
          "type": "pending"
        }
      },
      {
        "display_name": null,
        "index": 2,
        "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
        "machine_name": "beta",
        "operation": {
          "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [],
              "config_mounts": [],
              "entrypoint": [
                "sh",
                "-c",
                "echo api up; sleep 100000"
              ],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": {
                "state": "disabled"
              },
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "e0eccf29-1258-4c57-829f-2250eec054c4"
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
            "name": "api",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "ae4d79d2ba6b4c53b2297d5a40ef5c92",
            "update": {
              "monitor_millis": null,
              "order": "start_first"
            },
            "volumes": []
          },
          "type": "run_container"
        },
        "service_name": "api",
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
  "rolls_back": [
    2,
    3,
    4,
    5
  ],
  "runner": "cli-6d855c52-6332-40f3-96ff-f122dd5705a5",
  "runtime_names": {
    "api": "api",
    "web": "web"
  },
  "saved": 1,
  "services": [],
  "ships": [
    "web",
    "api"
  ],
  "started_at": 1792512994,
  "status": "applied",
  "upload": null
}
```

### deployment show unknown

```console
$ ployz deployment show 01ZZZZZZZZZZZZZZZZZZZZZZZZ
# exit 2
```

stderr:
```
error: Expected a Deployment ID or number
```

### set bad image

```console
$ ployz set web.image=alpine:does-not-exist
# exit 0
```

stdout:
```
Staged web.image in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy that fails (bad image, live progress) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Deploying #6 of shop/production
depth 0
  ✔ api  unchanged
  ✘ web  failed (0/2 ready)
─────────────────────────────────────────
1 unchanged · 1 failed · across 2 servers
error: web: Image alpine:does-not-exist not found, or its registry needs credentials.
  cause: Docker responded with status code 404: failed to resolve reference "docker.io/library/alpine:does-not-exist": docker.io/library/alpine:does-not-exist: not found
inspect: ployz get web.image --project shop --env production
undo: ployz history undo 4 --project shop --env production
# exit 1
```

### deploy that fails (pipe)

```console
$ ployz deploy
# exit 1
```

stderr:
```
Deploying #7 of shop/production
  api (depth 0): unchanged
  web (depth 0): waiting 0/2
  web (depth 0): failed (0/2 ready)
─────────────────────────────────────────
1 unchanged · 1 failed · across 2 servers
error: web: Image alpine:does-not-exist not found, or its registry needs credentials.
  cause: Docker responded with status code 404: failed to resolve reference "docker.io/library/alpine:does-not-exist": docker.io/library/alpine:does-not-exist: not found
inspect: ployz get web.image --project shop --env production
undo: ployz history undo 4 --project shop --env production
```

### restore image

```console
$ ployz set web.image=alpine:3.23.3
# exit 0
```

stdout:
```
Staged web.image in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy restore

```console
$ ployz deploy
# exit 0
```

stderr:
```
Nothing changed on the Servers; Deployment #8 shipped version #5.
```

