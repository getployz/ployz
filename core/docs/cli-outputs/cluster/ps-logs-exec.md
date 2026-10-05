### ps

```console
$ ployz ps
# exit 0
```

stdout:
```
CONTAINER ID	SERVICE	KIND	MACHINE	STATE
01479b917a80	api	service	3ba1dcbd6b854ea699b76f6ef296576d	running
24674546193d	web	service	0523a6681d894d128f69ea28b7ed62d5	running
e7023f44bd98	web	service	d7bb999f0d454dbcacc63d085021de34	running
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
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
      "container_id": "01479b917a80712965fb38a499c12d1ad0320cd1f48a628d7bccbdbcd8b0d39f",
      "created_at_unix_nanos": 1791164398559348164,
      "display_name": "api-5142",
      "effective_healthcheck": null,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "df2493a4-6fa5-4eb2-a66d-c198a6232238",
        "ployz.deployment.id": "bcb27adc-b55b-40dd-a889-0890a61a739e",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "dde2a84f73154cb189130b13832bb93c",
        "ployz.service.name": "api"
      },
      "machine_id": "3ba1dcbd6b854ea699b76f6ef296576d",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "MODE": "<redacted>",
            "PORT": "<redacted>"
          },
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
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.2.3",
      "container_id": "24674546193d6f0d79b6a3a6617d8157f0a6cab24ea21a995302b4e2986251bb",
      "created_at_unix_nanos": 1791164366433024335,
      "display_name": "web-c6ab",
      "effective_healthcheck": null,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405",
        "ployz.deployment.id": "f79c05bd-bd81-4927-850a-bb78255fa76b",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "e6b6cf18dc0346968d1ac00e03fd1af5",
        "ployz.service.name": "web"
      },
      "machine_id": "0523a6681d894d128f69ea28b7ed62d5",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "GREETING": "<redacted>",
            "PORT": "<redacted>"
          },
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
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.0.3",
      "container_id": "e7023f44bd985200f7822f44a7f596008fa76e14e01a94449b790961f9bc3ac9",
      "created_at_unix_nanos": 1791164382115175085,
      "display_name": "web-6b97",
      "effective_healthcheck": null,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405",
        "ployz.deployment.id": "f79c05bd-bd81-4927-850a-bb78255fa76b",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "e6b6cf18dc0346968d1ac00e03fd1af5",
        "ployz.service.name": "web"
      },
      "machine_id": "d7bb999f0d454dbcacc63d085021de34",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "GREETING": "<redacted>",
            "PORT": "<redacted>"
          },
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

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### logs web -n 3

```console
$ ployz logs web -n 3
# exit 0
```

stdout:
```
2026-10-05T01:41:07.849606203+00:00 gamma web/24674546193d | tick
2026-10-05T01:41:08.220677548+00:00 alpha web/e7023f44bd98 | tick
2026-10-05T01:41:09.851110917+00:00 gamma web/24674546193d | tick
2026-10-05T01:41:10.221895735+00:00 alpha web/e7023f44bd98 | tick
2026-10-05T01:41:11.852816207+00:00 gamma web/24674546193d | tick
2026-10-05T01:41:12.223140253+00:00 alpha web/e7023f44bd98 | tick
```

### logs (all Services) -n 2

```console
$ ployz logs -n 2
# exit 0
```

stdout:
```
2026-10-05T01:40:31.267212152+00:00 beta api/01479b917a80 | api up
2026-10-05T01:41:04.815832492+00:00 beta api/01479b917a80 | api up
2026-10-05T01:41:10.221895735+00:00 alpha web/e7023f44bd98 | tick
2026-10-05T01:41:11.852816207+00:00 gamma web/24674546193d | tick
2026-10-05T01:41:12.223140253+00:00 alpha web/e7023f44bd98 | tick
2026-10-05T01:41:13.854214762+00:00 gamma web/24674546193d | tick
```

### logs --json

```console
$ ployz logs web -n 2 --json | head -4
```

stdout (first lines):
```
{"container_id":"24674546193d6f0d79b6a3a6617d8157f0a6cab24ea21a995302b4e2986251bb","hook":null,"machine":"gamma","message":"tick\n","service":"web","service_id":"e6b6cf18dc0346968d1ac00e03fd1af5","stream":"stdout","timestamp":"2026-10-05T01:41:11.852816207+00:00"}
{"container_id":"e7023f44bd985200f7822f44a7f596008fa76e14e01a94449b790961f9bc3ac9","hook":null,"machine":"alpha","message":"tick\n","service":"web","service_id":"e6b6cf18dc0346968d1ac00e03fd1af5","stream":"stdout","timestamp":"2026-10-05T01:41:12.223140253+00:00"}
{"container_id":"24674546193d6f0d79b6a3a6617d8157f0a6cab24ea21a995302b4e2986251bb","hook":null,"machine":"gamma","message":"tick\n","service":"web","service_id":"e6b6cf18dc0346968d1ac00e03fd1af5","stream":"stdout","timestamp":"2026-10-05T01:41:13.854214762+00:00"}
{"container_id":"e7023f44bd985200f7822f44a7f596008fa76e14e01a94449b790961f9bc3ac9","hook":null,"machine":"alpha","message":"tick\n","service":"web","service_id":"e6b6cf18dc0346968d1ac00e03fd1af5","stream":"stdout","timestamp":"2026-10-05T01:41:14.224170739+00:00"}
```

### logs --deployment

```console
$ ployz logs --deployment c6da9b9c-d05f-4fb6-b555-00198287c73a -n 2
# exit 1
```

stderr:
```
no running container came from this Deployment
```

### logs unknown service

```console
$ ployz logs nope
# exit 1
```

stderr:
```
No running Service "nope"; ployz ps lists what's running
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
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
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
No running Service "nope"; ployz ps lists what's running
```

