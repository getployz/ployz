### ps

```console
$ ployz ps
# exit 0
```

stdout:
```
SERVICE	KIND	SERVER	STATE
api	service	beta	running
web	service	alpha	running
web	service	gamma	running
```

### ps --json

```console
$ ployz ps --json
# exit 0
```

stdout:
```
{
  "containers": [
    {
      "address": "10.210.1.3",
      "container_id": "b6ac931643a597b358fc657833d6e1d6f181f6d5daeee5e6da5ed777c1e2ed3c",
      "created_at_unix_nanos": 1792513034798033458,
      "display_name": "ployz-create-19d899b766d4cd259fb990cff41a01b087a903f0c77b5e8aab3f8167b421e19a",
      "effective_healthcheck": {
        "state": "disabled"
      },
      "gen": 2,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "e0eccf29-1258-4c57-829f-2250eec054c4",
        "ployz.creation.key": "4083f374-2bae-415f-a3ab-40eb9bc5f01e:db38871efdee299c:0",
        "ployz.deployment.id": "4083f374-2bae-415f-a3ab-40eb9bc5f01e",
        "ployz.gen": "2",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "ae4d79d2ba6b4c53b2297d5a40ef5c92",
        "ployz.service.name": "api"
      },
      "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "MODE": "<redacted>",
            "PORT": "<redacted>"
          },
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
      "rotation": "in",
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.0.3",
      "container_id": "0de25c6f017ea7ebd6e2e707692194812cd00af850617f2469de5fe502f0efcd",
      "created_at_unix_nanos": 1792513005737876210,
      "display_name": "ployz-create-1bcb95f2e806fab29774a6ab28fd2e896db443343f662b80ba0b32ffc86963a7",
      "effective_healthcheck": {
        "state": "disabled"
      },
      "gen": 2,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "608e4644-cd97-46d2-bb82-416f75554478",
        "ployz.creation.key": "3baf7c10-b3b4-4723-8bf0-d3cdf0012195:c95b16253744fc38:1",
        "ployz.deployment.id": "3baf7c10-b3b4-4723-8bf0-d3cdf0012195",
        "ployz.gen": "2",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "8df4db26a03241d2a935b04ab159deef",
        "ployz.service.name": "web"
      },
      "machine_id": "91e8bbd81cd34c59a186d49d3375bbe8",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "GREETING": "<redacted>",
            "PORT": "<redacted>"
          },
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
      "rotation": "in",
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.2.3",
      "container_id": "b966ab89d05cf424b852bde33f328cd7712ea3a0e49c693ac09afaca8ab1cbf2",
      "created_at_unix_nanos": 1792513005743021202,
      "display_name": "ployz-create-14f3ee91f0cdcf386f41ca376f50035a34f5bd11090b6c5bebea4ed84260647d",
      "effective_healthcheck": {
        "state": "disabled"
      },
      "gen": 2,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "608e4644-cd97-46d2-bb82-416f75554478",
        "ployz.creation.key": "3baf7c10-b3b4-4723-8bf0-d3cdf0012195:c95b16253744fc38:0",
        "ployz.deployment.id": "3baf7c10-b3b4-4723-8bf0-d3cdf0012195",
        "ployz.gen": "2",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "8df4db26a03241d2a935b04ab159deef",
        "ployz.service.name": "web"
      },
      "machine_id": "7f571356a2ff4c36abb2cd9a719ec8ec",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "GREETING": "<redacted>",
            "PORT": "<redacted>"
          },
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
      "rotation": "in",
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    }
  ],
  "failures": [],
  "omitted": []
}
```

### logs web -n 3

```console
$ ployz logs web -n 3
# exit 0
```

stdout:
```
2026-10-20T16:19:53.335329562+00:00 gamma web/b966ab89d05c | tick
2026-10-20T16:19:55.098423568+00:00 alpha web/0de25c6f017e | tick
2026-10-20T16:19:55.333234249+00:00 gamma web/b966ab89d05c | tick
```

### logs (all Services) -n 2

```console
$ ployz logs -n 2
# exit 0
```

stdout:
```
2026-10-20T16:19:55.098423568+00:00 alpha web/0de25c6f017e | tick
2026-10-20T16:19:55.333234249+00:00 gamma web/b966ab89d05c | tick
```

### logs --json

```console
$ ployz logs web -n 2 --json | head -4
```

stdout (first lines):
```
{"container":"0de25c6f017ea7ebd6e2e707692194812cd00af850617f2469de5fe502f0efcd","container_id":"0de25c6f017ea7ebd6e2e707692194812cd00af850617f2469de5fe502f0efcd","deployment":"3baf7c10-b3b4-4723-8bf0-d3cdf0012195","hook":null,"level":"info","line":"tick","machine":"alpha","message":"tick\n","namespace":"shop-production","replica":"ployz-create-1bcb95f2e806fab29774a6ab28fd2e896db443343f662b80ba0b32ffc86963a7","service":"web","stream":"stdout","timestamp":"2026-10-20T16:19:57.100923364+00:00","ts":"2026-10-20T16:19:57.100923364Z"}
{"container":"b966ab89d05cf424b852bde33f328cd7712ea3a0e49c693ac09afaca8ab1cbf2","container_id":"b966ab89d05cf424b852bde33f328cd7712ea3a0e49c693ac09afaca8ab1cbf2","deployment":"3baf7c10-b3b4-4723-8bf0-d3cdf0012195","hook":null,"level":"info","line":"tick","machine":"gamma","message":"tick\n","namespace":"shop-production","replica":"ployz-create-14f3ee91f0cdcf386f41ca376f50035a34f5bd11090b6c5bebea4ed84260647d","service":"web","stream":"stdout","timestamp":"2026-10-20T16:19:57.335177674+00:00","ts":"2026-10-20T16:19:57.335177674Z"}
```

### logs --deployment

```console
$ ployz logs --deployment 8040ba9e-75a4-4531-b2c2-1d0739001422 -n 2
# exit 0
```

stderr:
```
2026-10-20T16:17:11.459856830+00:00 alpha web/8a807ffcadac | exited with 137
2026-10-20T16:17:44.036980026+00:00 beta api/daee0729557f | exited with 137
```

### logs unknown service

```console
$ ployz logs nope
# exit 1
```

stderr:
```
error: No Service named nope in Environment production
valid: api, web
```

### exec

```console
$ ployz exec web -- echo hello from exec
# exit 0
```

stdout:
```
hello from exec
```

### exec --json

```console
$ ployz exec web --json -- echo hi
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### exec failing command

```console
$ ployz exec web -- sh -c echo\ oops\ \>\&2\;\ exit\ 3
# exit 3
```

stderr:
```
oops
```

### exec unknown service

```console
$ ployz exec nope -- true
# exit 1
```

stderr:
```
error: No Service named nope in Environment production
valid: api, web
```

