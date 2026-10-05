# Runtime commands through Cloud (real Server)

## status

### status

```console
$ ployz status
# exit 0
```

stdout:
```
Cloud: http://localhost:44581
Organization ada (token).
Environment shop/production.
0 staged changes.
```

### status --json

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
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 38
  },
  "identity": {
    "cloud": "http://localhost:44581",
    "credential": "token",
    "organization": {
      "id": "eccc0cff-2641-4745-8dac-fdee92352300",
      "slug": "ada"
    }
  },
  "scope": {
    "environment": null,
    "link": null,
    "project": null
  },
  "staged": {
    "changes": 0,
    "published": true,
    "version": "38:27:0.54"
  }
}
```

## ps

### ps

```console
$ ployz ps
# exit 0
```

stdout:
```
CONTAINER ID	SERVICE	KIND	MACHINE	STATE
210f3849f314	api	service	7784f1a17df144728819cecf47282904	running
bf055d4b5f1e	postgres	service	7784f1a17df144728819cecf47282904	running
651ea4cdb25d	web	service	7784f1a17df144728819cecf47282904	running
5e3a54fc5f3d	worker	service	7784f1a17df144728819cecf47282904	running
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### ps --json

```console
$ ployz --json ps
# exit 0
```

stdout:
```
{
  "containers": [
    {
      "address": "10.210.0.4",
      "container_id": "210f3849f314016fc3cedf845174206f152c716a443b9f3b2b88e0c6ce579082",
      "created_at_unix_nanos": 1791166257674611449,
      "display_name": "api-957e",
      "effective_healthcheck": null,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "a48be9de-5046-4ce1-ac09-736234a707d0",
        "org.opencontainers.image.created": "2025-03-13T14:25:39Z",
        "org.opencontainers.image.description": "Tiny Go webserver that prints OS information and HTTP request to output",
        "org.opencontainers.image.documentation": "https://github.com/traefik/whoami",
        "org.opencontainers.image.revision": "7e57190724ca7c0a74aa7f878b6cefe13b11994f",
        "org.opencontainers.image.source": "https://github.com/traefik/whoami",
        "org.opencontainers.image.title": "whoami",
        "org.opencontainers.image.url": "https://github.com/traefik/whoami",
        "org.opencontainers.image.version": "1.11.0",
        "ployz.deployment.id": "f9ef5d14-c944-4cd9-9501-4b89c5c9bdba",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "c9679360c13b401c9148032026092739",
        "ployz.service.name": "api"
      },
      "machine_id": "7784f1a17df144728819cecf47282904",
      "namespace": "shop-production",
      "resolved_spec": {
        "configs": [],
        "container": {
          "cap_add": [],
          "cap_drop": [],
          "command": [],
          "config_mounts": [],
          "entrypoint": [],
          "environment": {
            "DATABASE_URL": "<redacted>",
            "FEATURE_SEARCH": "<redacted>",
            "LOG_LEVEL": "<redacted>",
            "PORT": "<redacted>"
          },
          "extra_hosts": [],
          "healthcheck": null,
          "hostname": null,
          "image": "traefik/whoami:v1.11.0",
          "init": null,
          "labels": {
            "cloud.ployz.service.id": "a48be9de-5046-4ce1-ac09-736234a707d0"
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
        "service_id": "c9679360c13b401c9148032026092739",
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
      "address": "10.210.0.5",
      "container_id": "bf055d4b5f1eb3cd686e069492368e17283ee9ed266342677c98ea7cc4c4d49d",
      "created_at_unix_nanos": 1791166280637065069,
      "display_name": "postgres-8e0f",
      "effective_healthcheck": null,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc",
        "ployz.deployment.id": "f9ef5d14-c944-4cd9-9501-4b89c5c9bdba",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "fbdae59eeafb4457adde06ad16e83658",
        "ployz.service.name": "postgres"
      },
      "machine_id": "7784f1a17df144728819cecf47282904",
      "namespace": "shop-production",
      "resolved_spec": {
        "configs": [],
        "container": {
          "cap_add": [],
          "cap_drop": [],
          "command": [],
          "config_mounts": [],
          "entrypoint": [],
          "environment": {
            "PORT": "<redacted>",
            "POSTGRES_PASSWORD": "<redacted>"
          },
          "extra_hosts": [],
          "healthcheck": null,
          "hostname": null,
          "image": "postgres:16",
          "init": null,
          "labels": {
            "cloud.ployz.service.id": "bee1160e-e2bd-442a-b0ef-e98dc4debcdc"
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
        "mounts": [
          {
            "no_copy": false,
            "read_only": false,
            "subpath": null,
            "target": "/var/lib/postgresql/data",
            "volume": "vol-71802251-dbfa-42fa-9bc9-895e8385ece6"
          }
        ],
        "name": "postgres",
        "placement": {
          "constraints": []
        },
        "ports": [],
        "pre_deploy": null,
        "service_id": "fbdae59eeafb4457adde06ad16e83658",
        "update": {
          "monitor_millis": null,
          "order": "stop_first"
        },
        "volumes": [
          {
            "reference": "vol-71802251-dbfa-42fa-9bc9-895e8385ece6",
            "source": {
              "kind": "provisioned",
              "labels": {},
              "maximum_bytes": 5000000000,
              "name": "shop-production_vol-71802251-dbfa-42fa-9bc9-895e8385ece6",
              "scope": {
                "logical_name": "vol-71802251-dbfa-42fa-9bc9-895e8385ece6",
                "namespace": "shop-production"
              }
            }
          }
        ]
      },
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.0.3",
      "container_id": "651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1",
      "created_at_unix_nanos": 1791167005306093109,
      "display_name": "web-88f3",
      "effective_healthcheck": null,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
        "maintainer": "NGINX Docker Maintainers <docker-maint@nginx.com>",
        "ployz.deployment.id": "2e8e2cfb-03ba-43d1-9f82-5f8fa3dfacd9",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "6d5c9a3fb9d647d9a81bfe273e48dde6",
        "ployz.service.name": "web"
      },
      "machine_id": "7784f1a17df144728819cecf47282904",
      "namespace": "shop-production",
      "resolved_spec": {
        "configs": [],
        "container": {
          "cap_add": [],
          "cap_drop": [],
          "command": [],
          "config_mounts": [],
          "entrypoint": [],
          "environment": {
            "PORT": "<redacted>",
            "X": "<redacted>"
          },
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
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.0.6",
      "container_id": "5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878",
      "created_at_unix_nanos": 1791166288272260059,
      "display_name": "worker-949b",
      "effective_healthcheck": null,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "e45be4e1-f130-4720-a996-db5c0aaa9e0e",
        "org.opencontainers.image.created": "2024-08-22T08:05:56Z",
        "org.opencontainers.image.description": "Tiny Go webserver that prints OS information and HTTP request to output",
        "org.opencontainers.image.documentation": "https://github.com/traefik/whoami",
        "org.opencontainers.image.revision": "dec1ed84e37648285d4ddfae911344483c77906b",
        "org.opencontainers.image.source": "https://github.com/traefik/whoami",
        "org.opencontainers.image.title": "whoami",
        "org.opencontainers.image.url": "https://github.com/traefik/whoami",
        "org.opencontainers.image.version": "1.10.3",
        "ployz.deployment.id": "f9ef5d14-c944-4cd9-9501-4b89c5c9bdba",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "ec02437ef76a4d11a5af850934be313e",
        "ployz.service.name": "worker"
      },
      "machine_id": "7784f1a17df144728819cecf47282904",
      "namespace": "shop-production",
      "resolved_spec": {
        "configs": [],
        "container": {
          "cap_add": [],
          "cap_drop": [],
          "command": [],
          "config_mounts": [],
          "entrypoint": [],
          "environment": {
            "PORT": "<redacted>"
          },
          "extra_hosts": [],
          "healthcheck": null,
          "hostname": null,
          "image": "traefik/whoami:v1.10.3",
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

## logs

### logs one Service

```console
$ ployz logs web --tail 5
# exit 0
```

stderr:
```
2026-10-05T02:24:35.849952814+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: OS: Linux 6.8.0-146-generic
2026-10-05T02:24:35.850089081+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: getrlimit(RLIMIT_NOFILE): 1024:524288
2026-10-05T02:24:35.850278657+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker processes
2026-10-05T02:24:35.850448487+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker process 21
2026-10-05T02:24:35.850771795+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker process 22
```

### logs every Service

```console
$ ployz logs --tail 3
# exit 0
```

stderr:
```
2026-10-05T02:10:58.091856127+00:00 machine-1 api/210f3849f314 | 2026/10/05 02:10:58 Starting up on port 80
2026-10-05T02:11:23.270753526+00:00 machine-1 postgres/bf055d4b5f1e | 2026-10-05 02:11:23.270 UTC [1] LOG:  database system is ready to accept connections
2026-10-05T02:16:23.340682262+00:00 machine-1 postgres/bf055d4b5f1e | 2026-10-05 02:16:23.338 UTC [60] LOG:  checkpoint starting: time
2026-10-05T02:16:27.529925467+00:00 machine-1 postgres/bf055d4b5f1e | 2026-10-05 02:16:27.529 UTC [60] LOG:  checkpoint complete: wrote 44 buffers (0.3%); 0 WAL file(s) added, 0 removed, 0 recycled; write=4.146 s, sync=0.023 s, total=4.193 s; sync files=11, longest=0.004 s, average=0.003 s; distance=260 kB, estimate=260 kB; lsn=0/1533F08, redo lsn=0/1533ED0
2026-10-05T02:19:42.861185182+00:00 machine-1 api/210f3849f314 | 2026/10/05 02:19:42 Starting up on port 80
2026-10-05T02:24:35.850278657+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker processes
2026-10-05T02:24:35.850448487+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker process 21
2026-10-05T02:24:35.850771795+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker process 22
2026-10-05T02:24:42.527158057+00:00 machine-1 worker/5e3a54fc5f3d | 2026/10/05 02:24:42 Starting up on port 80
2026-10-05T02:24:46.369475605+00:00 machine-1 worker/5e3a54fc5f3d | 2026/10/05 02:24:46 Starting up on port 80
2026-10-05T02:24:50.121592889+00:00 machine-1 api/210f3849f314 | 2026/10/05 02:24:50 Starting up on port 80
2026-10-05T02:24:51.502047623+00:00 machine-1 worker/5e3a54fc5f3d | 2026/10/05 02:24:51 Starting up on port 80
```

### logs --json one Service

```console
$ ployz --json logs web --tail 3
# exit 0
```

stdout:
```
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:35 [notice] 1#1: start worker processes\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stderr","timestamp":"2026-10-05T02:24:35.850278657+00:00"}
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:35 [notice] 1#1: start worker process 21\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stderr","timestamp":"2026-10-05T02:24:35.850448487+00:00"}
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:35 [notice] 1#1: start worker process 22\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stderr","timestamp":"2026-10-05T02:24:35.850771795+00:00"}
```

### logs --json every Service

```console
$ ployz --json logs --tail 2
# exit 0
```

stdout:
```
{"container_id":"bf055d4b5f1eb3cd686e069492368e17283ee9ed266342677c98ea7cc4c4d49d","hook":null,"machine":"machine-1","message":"2026-10-05 02:16:23.338 UTC [60] LOG:  checkpoint starting: time\n","service":"postgres","service_id":"fbdae59eeafb4457adde06ad16e83658","stream":"stderr","timestamp":"2026-10-05T02:16:23.340682262+00:00"}
{"container_id":"bf055d4b5f1eb3cd686e069492368e17283ee9ed266342677c98ea7cc4c4d49d","hook":null,"machine":"machine-1","message":"2026-10-05 02:16:27.529 UTC [60] LOG:  checkpoint complete: wrote 44 buffers (0.3%); 0 WAL file(s) added, 0 removed, 0 recycled; write=4.146 s, sync=0.023 s, total=4.193 s; sync files=11, longest=0.004 s, average=0.003 s; distance=260 kB, estimate=260 kB; lsn=0/1533F08, redo lsn=0/1533ED0\n","service":"postgres","service_id":"fbdae59eeafb4457adde06ad16e83658","stream":"stderr","timestamp":"2026-10-05T02:16:27.529925467+00:00"}
{"container_id":"210f3849f314016fc3cedf845174206f152c716a443b9f3b2b88e0c6ce579082","hook":null,"machine":"machine-1","message":"2026/10/05 02:19:42 Starting up on port 80\n","service":"api","service_id":"c9679360c13b401c9148032026092739","stream":"stderr","timestamp":"2026-10-05T02:19:42.861185182+00:00"}
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:35 [notice] 1#1: start worker process 21\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stderr","timestamp":"2026-10-05T02:24:35.850448487+00:00"}
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:35 [notice] 1#1: start worker process 22\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stderr","timestamp":"2026-10-05T02:24:35.850771795+00:00"}
{"container_id":"5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:46 Starting up on port 80\n","service":"worker","service_id":"ec02437ef76a4d11a5af850934be313e","stream":"stderr","timestamp":"2026-10-05T02:24:46.369475605+00:00"}
{"container_id":"210f3849f314016fc3cedf845174206f152c716a443b9f3b2b88e0c6ce579082","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:50 Starting up on port 80\n","service":"api","service_id":"c9679360c13b401c9148032026092739","stream":"stderr","timestamp":"2026-10-05T02:24:50.121592889+00:00"}
{"container_id":"5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878","hook":null,"machine":"machine-1","message":"2026/10/05 02:24:51 Starting up on port 80\n","service":"worker","service_id":"ec02437ef76a4d11a5af850934be313e","stream":"stderr","timestamp":"2026-10-05T02:24:51.502047623+00:00"}
```

### logs --utc --since

```console
$ ployz logs api --utc --since 2h --tail 3
# exit 0
```

stderr:
```
2026-10-05T02:10:58.091856127+00:00 machine-1 api/210f3849f314 | 2026/10/05 02:10:58 Starting up on port 80
2026-10-05T02:19:42.861185182+00:00 machine-1 api/210f3849f314 | 2026/10/05 02:19:42 Starting up on port 80
2026-10-05T02:24:50.121592889+00:00 machine-1 api/210f3849f314 | 2026/10/05 02:24:50 Starting up on port 80
```

### logs --follow (stopped after 10s; exit 124 is that timeout)

```console
$ ployz logs web --follow --tail 2
# exit 124
```

stdout:
```
2026-10-05T02:25:46.441713438+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:25:46 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-05T02:25:48.072223859+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:25:48 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
```

stderr:
```
2026-10-05T02:24:35.850448487+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker process 21
2026-10-05T02:24:35.850771795+00:00 machine-1 web/651ea4cdb25d | 2026/10/05 02:24:35 [notice] 1#1: start worker process 22

Timed out after 10.0s
```

### logs --follow (stopped after 10s) (TTY)

```console
$ ployz logs web --follow --tail 2
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
2026-10-05T02:25:46.441713438+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:25:46 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-05T02:25:48.072223859+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:25:48 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-05T02:25:56.469902197+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:25:56 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-05T02:25:58.087490349+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:25:58 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
# exit 124

```

### logs --follow --json (stopped after 10s)

```console
$ ployz --json logs web --follow --tail 2
# exit 124
```

stdout:
```
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"::1 - - [05/Oct/2026:02:25:56 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stdout","timestamp":"2026-10-05T02:25:56.469902197+00:00"}
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"::1 - - [05/Oct/2026:02:25:58 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stdout","timestamp":"2026-10-05T02:25:58.087490349+00:00"}
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"::1 - - [05/Oct/2026:02:26:06 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stdout","timestamp":"2026-10-05T02:26:06.481100883+00:00"}
{"container_id":"651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1","hook":null,"machine":"machine-1","message":"::1 - - [05/Oct/2026:02:26:07 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","service":"web","service_id":"6d5c9a3fb9d647d9a81bfe273e48dde6","stream":"stdout","timestamp":"2026-10-05T02:26:07.954093028+00:00"}
```

stderr:
```

Timed out after 10.0s
```

### logs --deployment

```console
$ ployz logs --deployment 2e8e2cfb-03ba-43d1-9f82-5f8fa3dfacd9 --tail 2
# exit 0
```

stdout:
```
2026-10-05T02:26:06.481100883+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:26:06 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-05T02:26:07.954093028+00:00 machine-1 web/651ea4cdb25d | ::1 - - [05/Oct/2026:02:26:07 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
```

## exec

### exec a command

```console
$ ployz exec web -- nginx -v
# exit 0
```

stderr:
```
nginx version: nginx/1.27.5
```

### exec a command with output

```console
$ ployz exec web -- cat /etc/os-release
# exit 0
```

stdout:
```
NAME="Alpine Linux"
ID=alpine
VERSION_ID=3.21.3
PRETTY_NAME="Alpine Linux v3.21"
HOME_URL="https://alpinelinux.org/"
BUG_REPORT_URL="https://gitlab.alpinelinux.org/alpine/aports/-/issues"
```

### exec --json

```console
$ ployz --json exec web -- nginx -v
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### exec a failing command

```console
$ ployz exec web -- sh -c echo\ to-stderr\ \>\&2\;\ exit\ 7
# exit 7
```

stderr:
```
to-stderr
```

## service inspect

### service inspect

```console
$ ployz service inspect web
# exit 0
```

stdout:
```
Service web (8cb17782-fd07-45b5-9009-992bb7f6f24d)
Private DNS: web
Source: image
env={"X":"1"}
image="nginx:1.27-alpine"
maxRetries=10
privateDns="web"
replicas=1
restartPolicy="unless-stopped"
```

### service inspect --json

```console
$ ployz --json service inspect web
# exit 0
```

stdout:
```
{
  "change": null,
  "changes": [],
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 38
  },
  "id": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
  "lineage": "8cb17782-fd07-45b5-9009-992bb7f6f24d",
  "name": "web",
  "private_dns": "web",
  "row": "8cb17782-fd07-45b5-9009-992bb7f6f24d:node",
  "source": "image",
  "template": null,
  "values": {
    "env": {
      "X": "1"
    },
    "image": "nginx:1.27-alpine",
    "maxRetries": 10,
    "privateDns": "web",
    "replicas": 1,
    "restartPolicy": "unless-stopped"
  }
}
```

### service ls

```console
$ ployz service ls
# exit 0
```

stdout:
```
SERVICE	PRIVATE DNS	SOURCE	NEXT DEPLOY
api	api	image	-
postgres	postgres	image	-
web	web	image	-
worker	worker	image	-
```

## service restart, stop, start

### service restart

```console
$ ployz service restart web
# exit 0
```

stdout:
```
stop	shop-production/web	7784f1a17df144728819cecf47282904	651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1
start	shop-production/web	7784f1a17df144728819cecf47282904	651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### service restart --json

```console
$ ployz --json service restart web
# exit 0
```

stdout:
```
{
  "container_failures": [],
  "containers": [
    {
      "action": "stop",
      "container_id": "651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1",
      "machine_id": "7784f1a17df144728819cecf47282904",
      "service": "shop-production/web"
    },
    {
      "action": "start",
      "container_id": "651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1",
      "machine_id": "7784f1a17df144728819cecf47282904",
      "service": "shop-production/web"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
stop	shop-production/web	7784f1a17df144728819cecf47282904	651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1
start	shop-production/web	7784f1a17df144728819cecf47282904	651ea4cdb25dc654dba5912af9d684704da30081db23120600d8db355982acd1
```

### service stop

```console
$ ployz service stop worker
# exit 0
```

stdout:
```
stop	shop-production/worker	7784f1a17df144728819cecf47282904	5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### ps with a stopped Service

```console
$ ployz ps
# exit 0
```

stdout:
```
CONTAINER ID	SERVICE	KIND	MACHINE	STATE
210f3849f314	api	service	7784f1a17df144728819cecf47282904	running
bf055d4b5f1e	postgres	service	7784f1a17df144728819cecf47282904	running
651ea4cdb25d	web	service	7784f1a17df144728819cecf47282904	running
5e3a54fc5f3d	worker	service	7784f1a17df144728819cecf47282904	exited with code 2
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### service start

```console
$ ployz service start worker
# exit 0
```

stdout:
```
start	shop-production/worker	7784f1a17df144728819cecf47282904	5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### service stop --json

```console
$ ployz --json service stop worker
# exit 0
```

stdout:
```
{
  "container_failures": [],
  "containers": [
    {
      "action": "stop",
      "container_id": "5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878",
      "machine_id": "7784f1a17df144728819cecf47282904",
      "service": "shop-production/worker"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
stop	shop-production/worker	7784f1a17df144728819cecf47282904	5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878
```

### service start --json

```console
$ ployz --json service start worker
# exit 0
```

stdout:
```
{
  "container_failures": [],
  "containers": [
    {
      "action": "start",
      "container_id": "5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878",
      "machine_id": "7784f1a17df144728819cecf47282904",
      "service": "shop-production/worker"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
start	shop-production/worker	7784f1a17df144728819cecf47282904	5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878
```

### service restart two Services

```console
$ ployz service restart api worker
# exit 0
```

stdout:
```
stop	shop-production/api	7784f1a17df144728819cecf47282904	210f3849f314016fc3cedf845174206f152c716a443b9f3b2b88e0c6ce579082
start	shop-production/api	7784f1a17df144728819cecf47282904	210f3849f314016fc3cedf845174206f152c716a443b9f3b2b88e0c6ce579082
stop	shop-production/worker	7784f1a17df144728819cecf47282904	5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878
start	shop-production/worker	7784f1a17df144728819cecf47282904	5e3a54fc5f3df78c04a03d5bd77d14908241dd0a1b928df895d1854959149878
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

## server

### server ls

```console
$ ployz server ls
# exit 0
```

stdout:
```
ID	NAME	MEMBERSHIP	STORAGE	SUBNET	GATEWAY	PUBLIC IP	ENDPOINTS	HOSTNAME	DAEMON	DOCKER	OS	KERNEL	ARCH
7784f1a17df144728819cecf47282904	machine-1	up	Managed volumes available (7.0 GB, 10 MB used, 7.0 GB free)	10.210.0.0/24	10.210.0.1	203.0.113.10	10.223.175.62:51820,203.0.113.10:51820	machine-1	0.2.3	29.8.2	Ubuntu 24.04.5 LTS	6.8.0-146-generic	x86_64
```

### server ls --json

```console
$ ployz --json server ls
# exit 0
```

stdout:
```
{
  "failures": [],
  "omitted": [],
  "servers": [
    {
      "gateway": "10.210.0.1",
      "machine": {
        "accepts_builds": true,
        "accepts_ingress": true,
        "accepts_services": true,
        "advertised_endpoints": [
          "10.223.175.62:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "7784f1a17df144728819cecf47282904",
        "labels": {},
        "name": "machine-1",
        "public_ip": "203.0.113.10",
        "public_key": "h5tDeQ1ZYY3KVUIstoGUKsC7KCw+6r/i9J9lYeXN3HQ=",
        "runtime": {
          "architecture": "x86_64",
          "daemon_version": "0.2.3",
          "docker_version": "29.8.2",
          "hostname": "machine-1",
          "kernel_version": "6.8.0-146-generic",
          "memory_total_bytes": 2050584576,
          "os_pretty_name": "Ubuntu 24.04.5 LTS",
          "running_builds": 0
        },
        "subnet": "10.210.0.0/24"
      },
      "membership": "up",
      "rtt": null,
      "selected_endpoint": null,
      "storage": {
        "free_bytes": 6969446400,
        "size_bytes": 6979321856,
        "state": "pool",
        "used_bytes": 9875456
      }
    }
  ]
}
```

### server inspect

```console
$ ployz server inspect machine-1
# exit 0
```

stdout:
```
{
  "server": {
    "advertised_endpoints": [
      "10.223.175.62:51820",
      "203.0.113.10:51820"
    ],
    "id": "7784f1a17df144728819cecf47282904",
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "10.223.175.62:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "7784f1a17df144728819cecf47282904",
      "labels": {},
      "name": "machine-1",
      "public_ip": "203.0.113.10",
      "public_key": "h5tDeQ1ZYY3KVUIstoGUKsC7KCw+6r/i9J9lYeXN3HQ=",
      "runtime": {
        "architecture": "x86_64",
        "daemon_version": "0.2.3",
        "docker_version": "29.8.2",
        "hostname": "machine-1",
        "kernel_version": "6.8.0-146-generic",
        "memory_total_bytes": 2050584576,
        "os_pretty_name": "Ubuntu 24.04.5 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.0.0/24"
    },
    "management_clients": [
      "cli-59a792d0b8904f2f9c93b6d8c620",
      "cloud"
    ],
    "phase": "participating",
    "public_key": "h5tDeQ1ZYY3KVUIstoGUKsC7KCw+6r/i9J9lYeXN3HQ=",
    "rtts": [],
    "storage": {
      "free_bytes": 6969446400,
      "size_bytes": 6979321856,
      "state": "pool",
      "used_bytes": 9875456
    },
    "store_version": {
      "4388006ebd8542bb94d91959bf762650": 135
    },
    "telemetry": {
      "bridge": {
        "bridge_attached_endpoints": 5,
        "bridge_free_endpoints": 248,
        "bridge_usable_endpoints": 253
      },
      "host": {
        "cpu_count": 2,
        "docker_root_free_bytes": 14718599168,
        "docker_root_total_bytes": 24797970432,
        "load_average_milli": 490,
        "managed_containers": 5,
        "memory_available_bytes": 1493684224,
        "memory_total_bytes": 2050584576,
        "observed_at_unix_seconds": 1791167212
      },
      "scope": "full"
    }
  },
  "upgrade": null
}
```

### server inspect --json

```console
$ ployz --json server inspect machine-1
# exit 0
```

stdout:
```
{
  "server": {
    "advertised_endpoints": [
      "10.223.175.62:51820",
      "203.0.113.10:51820"
    ],
    "id": "7784f1a17df144728819cecf47282904",
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "10.223.175.62:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "7784f1a17df144728819cecf47282904",
      "labels": {},
      "name": "machine-1",
      "public_ip": "203.0.113.10",
      "public_key": "h5tDeQ1ZYY3KVUIstoGUKsC7KCw+6r/i9J9lYeXN3HQ=",
      "runtime": {
        "architecture": "x86_64",
        "daemon_version": "0.2.3",
        "docker_version": "29.8.2",
        "hostname": "machine-1",
        "kernel_version": "6.8.0-146-generic",
        "memory_total_bytes": 2050584576,
        "os_pretty_name": "Ubuntu 24.04.5 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.0.0/24"
    },
    "management_clients": [
      "cli-59a792d0b8904f2f9c93b6d8c620",
      "cloud"
    ],
    "phase": "participating",
    "public_key": "h5tDeQ1ZYY3KVUIstoGUKsC7KCw+6r/i9J9lYeXN3HQ=",
    "rtts": [],
    "storage": {
      "free_bytes": 6969446400,
      "size_bytes": 6979321856,
      "state": "pool",
      "used_bytes": 9875456
    },
    "store_version": {
      "4388006ebd8542bb94d91959bf762650": 135
    },
    "telemetry": {
      "bridge": {
        "bridge_attached_endpoints": 5,
        "bridge_free_endpoints": 248,
        "bridge_usable_endpoints": 253
      },
      "host": {
        "cpu_count": 2,
        "docker_root_free_bytes": 14718599168,
        "docker_root_total_bytes": 24797970432,
        "load_average_milli": 490,
        "managed_containers": 5,
        "memory_available_bytes": 1491394560,
        "memory_total_bytes": 2050584576,
        "observed_at_unix_seconds": 1791167215
      },
      "scope": "full"
    }
  },
  "upgrade": null
}
```

### server logs

```console
$ ployz server logs --tail 5
# exit 0
```

stdout:
```
2026-10-05T02:26:49.328899+00:00 machine-1 ployz | ployzd[1002]:  WARN failed closing path err=LastOpenPath
2026-10-05T02:26:50.320233+00:00 machine-1 ployz | ployzd[1002]:  WARN failed closing path err=LastOpenPath
2026-10-05T02:26:52.482447+00:00 machine-1 ployz | ployzd[1002]:  WARN failed closing path err=LastOpenPath
2026-10-05T02:26:53.776577+00:00 machine-1 ployz | ployzd[1002]:  WARN failed closing path err=LastOpenPath
2026-10-05T02:26:55.997450+00:00 machine-1 ployz | ployzd[1002]:  WARN failed closing path err=LastOpenPath
```

### server logs --json

```console
$ ployz --json server logs --tail 2
# exit 0
```

stdout:
```
{"container_id":null,"hook":null,"machine":"machine-1","message":"ployzd[1002]:  WARN failed closing path err=LastOpenPath\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-05T02:26:58.882654+00:00"}
{"container_id":null,"hook":null,"machine":"machine-1","message":"systemd[1]: ployz.service: Got notification message from PID 8233, but reception only permitted for main PID 1002\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-05T02:26:59.548388+00:00"}
```

