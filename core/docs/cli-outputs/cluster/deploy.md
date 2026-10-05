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
Staged new Service web in shop/production (revision 2).
```

### set web.startCommand

```console
$ ployz set web.startCommand=sh\ -c\ \'echo\ started\;\ while\ true\;\ do\ echo\ tick\;\ sleep\ 2\;\ done\'
# exit 0
```

stdout:
```
Staged web.startCommand in shop/production (revision 3).
```

### set web.replicas

```console
$ ployz set web.replicas=2
# exit 0
```

stdout:
```
Staged web.replicas in shop/production (revision 4).
```

### service add api

```console
$ ployz service add api --image alpine:3.23.3
# exit 0
```

stdout:
```
Staged new Service api in shop/production (revision 5).
```

### set api.startCommand

```console
$ ployz set api.startCommand=sh\ -c\ \'echo\ api\ up\;\ sleep\ 100000\'
# exit 0
```

stdout:
```
Staged api.startCommand in shop/production (revision 6).
```

### deploy --plan

```console
$ ployz deploy --plan
# exit 0
```

stdout:
```
web (create)
  web.startCommand: null -> "sh -c 'echo started; while true; do echo tick; sleep 2; done'"
  web.replicas: 1 -> 2
api (create)
  api.startCommand: null -> "sh -c 'echo api up; sleep 100000'"
Decided by the Servers when it runs: operations.
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
      "id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405",
      "lifecycle": "create",
      "name": "web",
      "row": "a085f2f9-e4df-439e-9d34-99e5b3ee7405:node",
      "settings": [
        {
          "after": "sh -c 'echo started; while true; do echo tick; sleep 2; done'",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "web.startCommand",
          "row": "a085f2f9-e4df-439e-9d34-99e5b3ee7405:startCommand"
        },
        {
          "after": 2,
          "before": 1,
          "canRestore": true,
          "kind": "update",
          "path": "web.replicas",
          "row": "a085f2f9-e4df-439e-9d34-99e5b3ee7405:replicas"
        }
      ],
      "type": "service"
    },
    {
      "comparison": "introduction",
      "data": null,
      "id": "df2493a4-6fa5-4eb2-a66d-c198a6232238",
      "lifecycle": "create",
      "name": "api",
      "row": "df2493a4-6fa5-4eb2-a66d-c198a6232238:node",
      "settings": [
        {
          "after": "sh -c 'echo api up; sleep 100000'",
          "before": null,
          "canRestore": true,
          "kind": "add",
          "path": "api.startCommand",
          "row": "df2493a4-6fa5-4eb2-a66d-c198a6232238:startCommand"
        }
      ],
      "type": "service"
    }
  ],
  "environment": {
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 6
  },
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

stdout:
```
queued: web pending, api pending
running: web pending, api pending
running: web deployed, api pending
applied: web deployed, api deployed
Deployment #1 of shop/production: applied
  web: deployed
  api: deployed
```

### set web.env (staged change)

```console
$ ployz set web.env.GREETING=hello
# exit 0
```

stdout:
```
Staged web.env.GREETING in shop/production (revision 7).
```

### deploy (live progress) (TTY)

```console
$ ployz deploy --message greeting
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
queued: web pending, api pending
running: web pending, api unchanged
applied: web deployed, api unchanged
Deployment #2 of shop/production: applied
  web: deployed
  api: unchanged
# exit 0

```

### set api.env (staged change)

```console
$ ployz set api.env.MODE=fast
# exit 0
```

stdout:
```
Staged api.env.MODE in shop/production (revision 8).
```

### deploy --json --events (result + NDJSON progress)

```console
$ ployz deploy --json --events /tmp/ployz-cap-home/events.ndjson
# exit 0
```

stdout:
```
{
  "admitted_at": 1791164398,
  "admitted_by": null,
  "builds": [],
  "ended_at": 1791164414,
  "environment": {
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 8
  },
  "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
  "id": "bcb27adc-b55b-40dd-a889-0890a61a739e",
  "in_flight": false,
  "message": null,
  "namespace": "shop-production",
  "next": "ployz deployment show bcb27adc-b55b-40dd-a889-0890a61a739e",
  "nodes": [
    {
      "id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405",
      "name": "web",
      "outcome": "unchanged",
      "type": "service"
    },
    {
      "id": "df2493a4-6fa5-4eb2-a66d-c198a6232238",
      "name": "api",
      "outcome": "deployed",
      "type": "service"
    }
  ],
  "number": 3,
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
        "display_name": "api-319b",
        "index": 0,
        "machine_id": "3ba1dcbd6b854ea699b76f6ef296576d",
        "machine_name": "beta",
        "operation": {
          "machine_id": "3ba1dcbd6b854ea699b76f6ef296576d",
          "old_container_id": "4956ccc13532e2c027d2b1855ff6b774a5168bc1b32192cc2c5912457829c486",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [
                "/bin/sh",
                "-c",
                "sh -c 'echo api up; sleep 100000'"
              ],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "df2493a4-6fa5-4eb2-a66d-c198a6232238"
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
            "name": "api",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "dde2a84f73154cb189130b13832bb93c",
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
  "remove": false,
  "runner": "cli-67db0733-916f-4391-b041-2a5548267975",
  "runtime_names": {
    "api": "api",
    "web": "web"
  },
  "saved": 3,
  "services": [],
  "started_at": 1791164398,
  "status": "applied",
  "upload": null
}
```

stderr:
```
queued: web pending, api pending
running: web unchanged, api pending
applied: web unchanged, api deployed
```

### deploy --events NDJSON (first 15 lines)

```
{"nodes":[{"id":"a085f2f9-e4df-439e-9d34-99e5b3ee7405","name":"web","outcome":"pending","type":"service"},{"id":"df2493a4-6fa5-4eb2-a66d-c198a6232238","name":"api","outcome":"pending","type":"service"}],"status":"queued","type":"deployment"}
{"nodes":[{"id":"a085f2f9-e4df-439e-9d34-99e5b3ee7405","name":"web","outcome":"unchanged","type":"service"},{"id":"df2493a4-6fa5-4eb2-a66d-c198a6232238","name":"api","outcome":"pending","type":"service"}],"status":"running","type":"deployment"}
{"nodes":[{"id":"a085f2f9-e4df-439e-9d34-99e5b3ee7405","name":"web","outcome":"unchanged","type":"service"},{"id":"df2493a4-6fa5-4eb2-a66d-c198a6232238","name":"api","outcome":"deployed","type":"service"}],"status":"applied","type":"deployment"}
```

### deploy with nothing staged

```console
$ ployz deploy
# exit 0
```

stdout:
```
queued: web pending, api pending
applied: web unchanged, api unchanged
Deployment #4 of shop/production: applied
  web: unchanged
  api: unchanged
```

### deploy --detach

```console
$ ployz deploy --detach
# exit 2
```

stderr:
```
The hidden local Store runs Deployments in this process, so it can't detach
```

### deployment ls

```console
$ ployz deployment ls
# exit 0
```

stdout:
```
#6 queued Saved revision 3 fa44a400-58b5-4a09-add8-125931cb4c32
#5 applied Saved revision 3 9e382b23-ddf2-4918-9203-7d5f813c5b3a
#4 applied Saved revision 3 a227c339-958c-4c48-9093-e038596e5449
#3 applied Saved revision 3 bcb27adc-b55b-40dd-a889-0890a61a739e
#2 applied Saved revision 2 f79c05bd-bd81-4927-850a-bb78255fa76b
#1 applied Saved revision 1 c6da9b9c-d05f-4fb6-b555-00198287c73a
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
      "admitted_at": 1791164416,
      "admitted_by": null,
      "ended_at": null,
      "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
      "id": "fa44a400-58b5-4a09-add8-125931cb4c32",
      "in_flight": true,
      "message": null,
      "number": 6,
      "outcome": null,
      "remove": false,
      "runner": null,
      "saved": 3,
      "services": [],
      "started_at": null,
      "status": "queued",
      "upload": null
    },
    {
      "admitted_at": 1791164415,
      "admitted_by": null,
      "ended_at": 1791164415,
      "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
      "id": "9e382b23-ddf2-4918-9203-7d5f813c5b3a",
      "in_flight": false,
      "message": null,
      "number": 5,
      "outcome": {
        "reason": null,
        "summary": {
          "completed": 0,
          "type": "success"
        },
        "type": "executed"
      },
      "remove": false,
      "runner": "cli-f2022279-d164-4144-861e-04f6af94f0d7",
      "saved": 3,
      "services": [],
      "started_at": 1791164415,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1791164414,
      "admitted_by": null,
      "ended_at": 1791164414,
      "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
      "id": "a227c339-958c-4c48-9093-e038596e5449",
      "in_flight": false,
      "message": null,
      "number": 4,
      "outcome": {
        "reason": null,
        "summary": {
          "completed": 0,
          "type": "success"
        },
        "type": "executed"
      },
      "remove": false,
      "runner": "cli-fdc72a26-39b7-4db5-b71b-04f1fc5e834b",
      "saved": 3,
      "services": [],
      "started_at": 1791164414,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1791164398,
      "admitted_by": null,
      "ended_at": 1791164414,
      "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
      "id": "bcb27adc-b55b-40dd-a889-0890a61a739e",
      "in_flight": false,
      "message": null,
      "number": 3,
      "outcome": {
        "reason": null,
        "summary": {
          "completed": 1,
          "type": "success"
        },
        "type": "executed"
      },
      "remove": false,
      "runner": "cli-67db0733-916f-4391-b041-2a5548267975",
      "saved": 3,
      "services": [],
      "started_at": 1791164398,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1791164366,
      "admitted_by": null,
      "ended_at": 1791164397,
      "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
      "id": "f79c05bd-bd81-4927-850a-bb78255fa76b",
      "in_flight": false,
      "message": "greeting",
      "number": 2,
      "outcome": {
        "reason": null,
        "summary": {
          "completed": 2,
          "type": "success"
        },
        "type": "executed"
      },
      "remove": false,
      "runner": "cli-54e388ee-088d-412b-a549-8aed70b2d1ca",
      "saved": 2,
      "services": [],
      "started_at": 1791164366,
      "status": "applied",
      "upload": null
    },
    {
      "admitted_at": 1791164348,
      "admitted_by": null,
      "ended_at": 1791164365,
      "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
      "id": "c6da9b9c-d05f-4fb6-b555-00198287c73a",
      "in_flight": false,
      "message": "first deploy",
      "number": 1,
      "outcome": {
        "reason": null,
        "summary": {
          "completed": 3,
          "type": "success"
        },
        "type": "executed"
      },
      "remove": false,
      "runner": "cli-357b3702-5cb9-4c71-8da2-e0605191b24b",
      "saved": 1,
      "services": [],
      "started_at": 1791164348,
      "status": "applied",
      "upload": null
    }
  ],
  "environment": {
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 8
  },
  "next_cursor": null
}
```

### deployment show

```console
$ ployz deployment show c6da9b9c-d05f-4fb6-b555-00198287c73a
# exit 0
```

stdout:
```
Deployment #1 of shop/production: applied
  web: deployed
  api: deployed
```

### deployment show --json

```console
$ ployz deployment show c6da9b9c-d05f-4fb6-b555-00198287c73a --json
# exit 0
```

stdout:
```
{
  "admitted_at": 1791164348,
  "admitted_by": null,
  "builds": [],
  "ended_at": 1791164365,
  "environment": {
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 8
  },
  "environment_id": "52712cb1-5621-43aa-987b-900e655738a0",
  "id": "c6da9b9c-d05f-4fb6-b555-00198287c73a",
  "in_flight": false,
  "message": "first deploy",
  "namespace": "shop-production",
  "nodes": [
    {
      "id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405",
      "name": "web",
      "outcome": "deployed",
      "type": "service"
    },
    {
      "id": "df2493a4-6fa5-4eb2-a66d-c198a6232238",
      "name": "api",
      "outcome": "deployed",
      "type": "service"
    }
  ],
  "number": 1,
  "outcome": {
    "reason": null,
    "summary": {
      "completed": 3,
      "type": "success"
    },
    "type": "executed"
  },
  "preview": {
    "namespace": "shop-production",
    "operations": [
      {
        "display_name": null,
        "index": 0,
        "machine_id": "0523a6681d894d128f69ea28b7ed62d5",
        "machine_name": "gamma",
        "operation": {
          "machine_id": "0523a6681d894d128f69ea28b7ed62d5",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [
                "/bin/sh",
                "-c",
                "sh -c 'echo started; while true; do echo tick; sleep 2; done'"
              ],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405"
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
              "replicas": 2
            },
            "mounts": [],
            "name": "web",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "e6b6cf18dc0346968d1ac00e03fd1af5",
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
        "machine_id": "d7bb999f0d454dbcacc63d085021de34",
        "machine_name": "alpha",
        "operation": {
          "machine_id": "d7bb999f0d454dbcacc63d085021de34",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [
                "/bin/sh",
                "-c",
                "sh -c 'echo started; while true; do echo tick; sleep 2; done'"
              ],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405"
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
              "replicas": 2
            },
            "mounts": [],
            "name": "web",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "e6b6cf18dc0346968d1ac00e03fd1af5",
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
        "machine_id": "3ba1dcbd6b854ea699b76f6ef296576d",
        "machine_name": "beta",
        "operation": {
          "machine_id": "3ba1dcbd6b854ea699b76f6ef296576d",
          "skip_health_monitor": false,
          "spec": {
            "configs": [],
            "container": {
              "cap_add": [],
              "cap_drop": [],
              "command": [
                "/bin/sh",
                "-c",
                "sh -c 'echo api up; sleep 100000'"
              ],
              "config_mounts": [],
              "entrypoint": [],
              "environment": {},
              "extra_hosts": [],
              "healthcheck": null,
              "hostname": null,
              "image": "alpine:3.23.3",
              "init": null,
              "labels": {
                "cloud.ployz.service.id": "df2493a4-6fa5-4eb2-a66d-c198a6232238"
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
            "name": "api",
            "placement": {
              "constraints": []
            },
            "ports": [],
            "pre_deploy": null,
            "service_id": "dde2a84f73154cb189130b13832bb93c",
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
  "remove": false,
  "runner": "cli-357b3702-5cb9-4c71-8da2-e0605191b24b",
  "runtime_names": {
    "api": "api",
    "web": "web"
  },
  "saved": 1,
  "services": [],
  "started_at": 1791164348,
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
Expected a Deployment ID or number
```

### set bad image

```console
$ ployz set web.image=alpine:does-not-exist
# exit 0
```

stdout:
```
Staged web.image in shop/production (revision 9).
```

### deploy that fails (bad image, live progress) (TTY)

```console
$ ployz deploy
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
queued: web pending, api pending
failed: web failed, api unchanged
Deployment #7 of shop/production: failed
  web: create Container failed: Docker operation failed: Docker responded with status code 404: failed to resolve reference "docker.io/library/alpine:does-not-exist": docker.io/library/alpine:does-not-exist: not found
  web: failed
  api: unchanged
# exit 3

```

### deploy that fails (pipe)

```console
$ ployz deploy
# exit 3
```

stdout:
```
queued: web pending, api pending
failed: web failed, api unchanged
Deployment #8 of shop/production: failed
  web: create Container failed: Docker operation failed: Docker responded with status code 404: failed to resolve reference "docker.io/library/alpine:does-not-exist": docker.io/library/alpine:does-not-exist: not found
  web: failed
  api: unchanged
```

### restore image

```console
$ ployz set web.image=alpine:3.23.3
# exit 0
```

stdout:
```
Staged web.image in shop/production (revision 10).
```

### deploy restore

```console
$ ployz deploy
# exit 0
```

stdout:
```
queued: web pending, api pending
applied: web unchanged, api unchanged
Deployment #9 of shop/production: applied
  web: unchanged
  api: unchanged
```

