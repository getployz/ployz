# Runtime commands through Cloud (real Server)

## status

### status

```console
$ ployz status
# exit 0
```

stdout:
```
cloud = http://localhost:38265
organization = ada
signed in as = token
environment = shop/production
linked from = <run-dir>/workspace
staged = 0 changes
needs attention = sentry/config.yml references redis, which was removed
```

stderr:
```
next: ployz config put sentry config.yml --from PATH
```

### status --json

```console
$ ployz --json status
# exit 0
```

stdout:
```
{
  "attention": [
    {
      "message": "sentry/config.yml references redis, which was removed",
      "next": "ployz config put sentry config.yml --from PATH",
      "reason": "broken_reference"
    }
  ],
  "deploying": [],
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 26
  },
  "identity": {
    "cloud": "http://localhost:38265",
    "credential": "token",
    "organization": {
      "id": "c5bb4022-ae5d-439a-b670-d567afb5f40a",
      "slug": "ada"
    }
  },
  "next": "ployz config put sentry config.yml --from PATH",
  "scope": {
    "environment": {
      "name": "production",
      "source": "link"
    },
    "link": "<run-dir>/workspace",
    "project": {
      "name": "shop",
      "source": "link"
    }
  },
  "staged": {
    "changes": 0,
    "saved": true,
    "version": "26:13:0.23"
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
SERVICE	KIND	SERVER	STATE
api	service	machine-1	running
postgres	service	machine-1	running
web	service	machine-1	running
worker	service	machine-1	running
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
      "address": "10.210.0.3",
      "container_id": "7561bf62883c4c087d895359a3b47c61c536366117fd8b5ebb29885538fad5a7",
      "created_at_unix_nanos": 1792514779813926794,
      "display_name": "ployz-create-99d54d331aaf8f4f72795e07d2691fea44d20fd6e06ee1d0bb1d5194de6088a3",
      "effective_healthcheck": {
        "state": "disabled"
      },
      "gen": 1,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "254b1264-6d96-4e34-86d2-795920ddab60",
        "org.opencontainers.image.created": "2025-03-13T14:25:39Z",
        "org.opencontainers.image.description": "Tiny Go webserver that prints OS information and HTTP request to output",
        "org.opencontainers.image.documentation": "https://github.com/traefik/whoami",
        "org.opencontainers.image.revision": "7e57190724ca7c0a74aa7f878b6cefe13b11994f",
        "org.opencontainers.image.source": "https://github.com/traefik/whoami",
        "org.opencontainers.image.title": "whoami",
        "org.opencontainers.image.url": "https://github.com/traefik/whoami",
        "org.opencontainers.image.version": "1.11.0",
        "ployz.creation.key": "47daab3f-5f9c-4ced-86f9-702188c7b044:ea5692c0a16e2fc9:1",
        "ployz.deployment.id": "47daab3f-5f9c-4ced-86f9-702188c7b044",
        "ployz.gen": "1",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "34a4b9e6931b4b8bb57920f343b4b784",
        "ployz.service.name": "api"
      },
      "machine_id": "c109917d492d460a994dbf4b43162146",
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
            "PORT": "<redacted>",
            "SECRET_KEY": "<redacted>"
          },
          "extra_hosts": [],
          "healthcheck": {
            "state": "disabled"
          },
          "hostname": null,
          "image": "traefik/whoami:v1.11.0",
          "init": null,
          "labels": {
            "cloud.ployz.service.id": "254b1264-6d96-4e34-86d2-795920ddab60"
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
        "service_id": "34a4b9e6931b4b8bb57920f343b4b784",
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
      "address": "10.210.0.5",
      "container_id": "3f99a088f735aa494b5f02a7d0dcf4dcecd6d9cadcf6afa02eea650c6584442a",
      "created_at_unix_nanos": 1792514781942729773,
      "display_name": "ployz-create-bdd81f7a0bd2d741c34fad08ddc06d7a5e124f1a8d7fcc99f41c3ff45b185ac7",
      "effective_healthcheck": {
        "state": "disabled"
      },
      "gen": 1,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "796ce242-5b1d-451d-8804-42de83e14920",
        "ployz.creation.key": "47daab3f-5f9c-4ced-86f9-702188c7b044:ea5692c0a16e2fc9:3",
        "ployz.deployment.id": "47daab3f-5f9c-4ced-86f9-702188c7b044",
        "ployz.gen": "1",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "dcb21932452441cf9eaed62ec83650b6",
        "ployz.service.name": "postgres"
      },
      "machine_id": "c109917d492d460a994dbf4b43162146",
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
          "healthcheck": {
            "state": "disabled"
          },
          "hostname": null,
          "image": "postgres:16",
          "init": null,
          "labels": {
            "cloud.ployz.service.id": "796ce242-5b1d-451d-8804-42de83e14920"
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
        "mounts": [
          {
            "no_copy": false,
            "read_only": false,
            "subpath": null,
            "target": "/var/lib/postgresql/data",
            "volume": "vol-2a748c8b-32bc-433a-be41-3420710e6b7c"
          }
        ],
        "name": "postgres",
        "placement": {
          "constraints": []
        },
        "ports": [],
        "pre_deploy": null,
        "service_id": "dcb21932452441cf9eaed62ec83650b6",
        "update": {
          "monitor_millis": null,
          "order": "stop_first"
        },
        "volumes": [
          {
            "reference": "vol-2a748c8b-32bc-433a-be41-3420710e6b7c",
            "source": {
              "kind": "provisioned",
              "labels": {},
              "maximum_bytes": 5000000000,
              "name": "shop-production_vol-2a748c8b-32bc-433a-be41-3420710e6b7c",
              "scope": {
                "logical_name": "vol-2a748c8b-32bc-433a-be41-3420710e6b7c",
                "namespace": "shop-production"
              }
            }
          }
        ]
      },
      "rotation": "in",
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.0.4",
      "container_id": "c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72",
      "created_at_unix_nanos": 1792515331545564368,
      "display_name": "ployz-create-0d0630aa97722410328d4ced3cc36357ecccedb8c779bc363640cdf20da1d013",
      "effective_healthcheck": {
        "state": "disabled"
      },
      "gen": 9,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
        "maintainer": "NGINX Docker Maintainers <docker-maint@nginx.com>",
        "ployz.creation.key": "9f056dff-7720-4f2e-bbbb-724c39e92221:d2ff0324918f06c6:0",
        "ployz.deployment.id": "9f056dff-7720-4f2e-bbbb-724c39e92221",
        "ployz.gen": "9",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "0bc3883482ba480298f83b694d408c4d",
        "ployz.service.name": "web"
      },
      "machine_id": "c109917d492d460a994dbf4b43162146",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "PORT": "<redacted>"
          },
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
      "rotation": "in",
      "runtime": {
        "health": "not_configured",
        "state": "running"
      }
    },
    {
      "address": "10.210.0.6",
      "container_id": "7dfd628663aeea6f3b1c5e43b411d2830025a329502895be70a72061404a9b45",
      "created_at_unix_nanos": 1792514898935457246,
      "display_name": "ployz-create-9b7dc528e04e7ac864f3e588153a9b6f541d7ed7dc8567995cc5e853e4b43946",
      "effective_healthcheck": {
        "state": "disabled"
      },
      "gen": 1,
      "kind": "service_container",
      "labels": {
        "cloud.ployz.service.id": "eb6735e8-bf91-4e42-9348-e465eaea151e",
        "org.opencontainers.image.created": "2024-08-22T08:05:56Z",
        "org.opencontainers.image.description": "Tiny Go webserver that prints OS information and HTTP request to output",
        "org.opencontainers.image.documentation": "https://github.com/traefik/whoami",
        "org.opencontainers.image.revision": "dec1ed84e37648285d4ddfae911344483c77906b",
        "org.opencontainers.image.source": "https://github.com/traefik/whoami",
        "org.opencontainers.image.title": "whoami",
        "org.opencontainers.image.url": "https://github.com/traefik/whoami",
        "org.opencontainers.image.version": "1.10.3",
        "ployz.creation.key": "af950c5d-77ac-4f26-8a8c-06af1018321a:a9d876b649f5f67a:1",
        "ployz.deployment.id": "af950c5d-77ac-4f26-8a8c-06af1018321a",
        "ployz.gen": "1",
        "ployz.managed": "",
        "ployz.namespace": "shop-production",
        "ployz.service.id": "b3b19a0846b2400d8920ccdb576dc8d0",
        "ployz.service.name": "worker"
      },
      "machine_id": "c109917d492d460a994dbf4b43162146",
      "namespace": "shop-production",
      "resolved_spec": {
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
          "environment": {
            "PORT": "<redacted>"
          },
          "extra_hosts": [],
          "healthcheck": {
            "state": "disabled"
          },
          "hostname": null,
          "image": "traefik/whoami:v1.10.3",
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

## logs

### logs one Service

```console
$ ployz logs web --tail 5
# exit 0
```

stderr:
```
2026-10-20T16:55:43.649308042+00:00 machine-1 web/7093e0b04215 | 2026/10/20 16:55:43 [notice] 1#1: signal 17 (SIGCHLD) received from 32
2026-10-20T16:55:43.649383283+00:00 machine-1 web/7093e0b04215 | 2026/10/20 16:55:43 [notice] 1#1: worker process 31 exited with code 0
2026-10-20T16:55:43.649392290+00:00 machine-1 web/7093e0b04215 | 2026/10/20 16:55:43 [notice] 1#1: worker process 32 exited with code 0
2026-10-20T16:55:43.649398812+00:00 machine-1 web/7093e0b04215 | 2026/10/20 16:55:43 [notice] 1#1: exit
2026-10-20T16:55:43.711050504+00:00 machine-1 web/7093e0b04215 | exited with 0
```

### logs every Service

```console
$ ployz logs --tail 3
# exit 0
```

stderr:
```
2026-10-20T16:55:43.649392290+00:00 machine-1 web/7093e0b04215 | 2026/10/20 16:55:43 [notice] 1#1: worker process 32 exited with code 0
2026-10-20T16:55:43.649398812+00:00 machine-1 web/7093e0b04215 | 2026/10/20 16:55:43 [notice] 1#1: exit
2026-10-20T16:55:43.711050504+00:00 machine-1 web/7093e0b04215 | exited with 0
```

### logs --json one Service

```console
$ ployz --json logs web --tail 3
# exit 0
```

stdout:
```
{"container":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","container_id":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","deployment":"928a23ec-4852-48b4-b8be-d500434c02e5","hook":null,"level":"info","line":"2026/10/20 16:55:43 [notice] 1#1: worker process 32 exited with code 0","machine":"machine-1","message":"2026/10/20 16:55:43 [notice] 1#1: worker process 32 exited with code 0\n","namespace":"shop-production","replica":"ployz-create-f50bc49694062b4e617332172e8cf88a12b3d9e6bd18194cdd5937fdbd76e958","service":"web","stream":"stderr","timestamp":"2026-10-20T16:55:43.649392290+00:00","ts":"2026-10-20T16:55:43.649392290Z"}
{"container":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","container_id":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","deployment":"928a23ec-4852-48b4-b8be-d500434c02e5","hook":null,"level":"info","line":"2026/10/20 16:55:43 [notice] 1#1: exit","machine":"machine-1","message":"2026/10/20 16:55:43 [notice] 1#1: exit\n","namespace":"shop-production","replica":"ployz-create-f50bc49694062b4e617332172e8cf88a12b3d9e6bd18194cdd5937fdbd76e958","service":"web","stream":"stderr","timestamp":"2026-10-20T16:55:43.649398812+00:00","ts":"2026-10-20T16:55:43.649398812Z"}
{"container":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","container_id":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","deployment":"928a23ec-4852-48b4-b8be-d500434c02e5","exit":{"code":0,"oom_killed":false},"hook":null,"machine":"machine-1","namespace":"shop-production","replica":"ployz-create-f50bc49694062b4e617332172e8cf88a12b3d9e6bd18194cdd5937fdbd76e958","service":"web","timestamp":"2026-10-20T16:55:43.711050504+00:00","ts":"2026-10-20T16:55:43.711050504Z"}
```

### logs --json every Service

```console
$ ployz --json logs --tail 2
# exit 0
```

stdout:
```
{"container":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","container_id":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","deployment":"928a23ec-4852-48b4-b8be-d500434c02e5","hook":null,"level":"info","line":"2026/10/20 16:55:43 [notice] 1#1: exit","machine":"machine-1","message":"2026/10/20 16:55:43 [notice] 1#1: exit\n","namespace":"shop-production","replica":"ployz-create-f50bc49694062b4e617332172e8cf88a12b3d9e6bd18194cdd5937fdbd76e958","service":"web","stream":"stderr","timestamp":"2026-10-20T16:55:43.649398812+00:00","ts":"2026-10-20T16:55:43.649398812Z"}
{"container":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","container_id":"7093e0b04215246d38163ccd4812d30fccb026423cef07560f87d54b11c4c19b","deployment":"928a23ec-4852-48b4-b8be-d500434c02e5","exit":{"code":0,"oom_killed":false},"hook":null,"machine":"machine-1","namespace":"shop-production","replica":"ployz-create-f50bc49694062b4e617332172e8cf88a12b3d9e6bd18194cdd5937fdbd76e958","service":"web","timestamp":"2026-10-20T16:55:43.711050504+00:00","ts":"2026-10-20T16:55:43.711050504Z"}
```

### logs --utc --since

```console
$ ployz logs api --utc --since 2h --tail 3
# exit 0
```

stderr:
```
2026-10-20T16:46:21.544183687+00:00 machine-1 api/7561bf62883c | 2026/10/20 16:46:21 Starting up on port 80
```

### logs --follow (stopped after 10s; exit 124 is that timeout)

```console
$ ployz logs web --follow --tail 2
# exit 124
```

stdout:
```
2026-10-20T16:56:24.993412207+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:24 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-20T16:56:27.427952019+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:27 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
```

stderr:
```
2026-10-20T16:55:43.649398812+00:00 machine-1 web/7093e0b04215 | 2026/10/20 16:55:43 [notice] 1#1: exit
2026-10-20T16:55:43.711050504+00:00 machine-1 web/7093e0b04215 | exited with 0

Timed out after 10.0s
```

### logs --follow (stopped after 10s) (TTY)

```console
$ ployz logs web --follow --tail 2
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
2026-10-20T16:56:24.993412207+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:24 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-20T16:56:27.427952019+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:27 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-20T16:56:36.516028097+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:36 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-20T16:56:38.961923106+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:38 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
# exit 124
```

### logs --follow --json (stopped after 10s)

```console
$ ployz --json logs web --follow --tail 2
# exit 124
```

stdout:
```
{"container":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","container_id":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","deployment":"9f056dff-7720-4f2e-bbbb-724c39e92221","hook":null,"level":"info","line":"::1 - - [20/Oct/2026:16:56:36 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"","machine":"machine-1","message":"::1 - - [20/Oct/2026:16:56:36 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","namespace":"shop-production","replica":"ployz-create-0d0630aa97722410328d4ced3cc36357ecccedb8c779bc363640cdf20da1d013","service":"web","stream":"stdout","timestamp":"2026-10-20T16:56:36.516028097+00:00","ts":"2026-10-20T16:56:36.516028097Z"}
{"container":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","container_id":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","deployment":"9f056dff-7720-4f2e-bbbb-724c39e92221","hook":null,"level":"info","line":"::1 - - [20/Oct/2026:16:56:38 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"","machine":"machine-1","message":"::1 - - [20/Oct/2026:16:56:38 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","namespace":"shop-production","replica":"ployz-create-0d0630aa97722410328d4ced3cc36357ecccedb8c779bc363640cdf20da1d013","service":"web","stream":"stdout","timestamp":"2026-10-20T16:56:38.961923106+00:00","ts":"2026-10-20T16:56:38.961923106Z"}
{"container":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","container_id":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","deployment":"9f056dff-7720-4f2e-bbbb-724c39e92221","hook":null,"level":"info","line":"::1 - - [20/Oct/2026:16:56:45 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"","machine":"machine-1","message":"::1 - - [20/Oct/2026:16:56:45 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","service":"web","service_id":"0bc3883482ba480298f83b694d408c4d","stream":"stdout","timestamp":"2026-10-20T16:56:45.860111844+00:00","ts":"2026-10-20T16:56:45.860111844Z"}
{"container":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","container_id":"c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72","deployment":"9f056dff-7720-4f2e-bbbb-724c39e92221","hook":null,"level":"info","line":"::1 - - [20/Oct/2026:16:56:48 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"","machine":"machine-1","message":"::1 - - [20/Oct/2026:16:56:48 +0000] \"GET / HTTP/1.1\" 200 615 \"-\" \"Wget\" \"-\"\n","service":"web","service_id":"0bc3883482ba480298f83b694d408c4d","stream":"stdout","timestamp":"2026-10-20T16:56:48.366259835+00:00","ts":"2026-10-20T16:56:48.366259835Z"}
```

stderr:
```

Timed out after 10.0s
```

### logs --deployment

```console
$ ployz logs --deployment 9f056dff-7720-4f2e-bbbb-724c39e92221 --tail 2
# exit 0
```

stdout:
```
2026-10-20T16:56:45.860111844+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:45 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
2026-10-20T16:56:48.366259835+00:00 machine-1 web/c8361accee66 | ::1 - - [20/Oct/2026:16:56:48 +0000] "GET / HTTP/1.1" 200 615 "-" "Wget" "-"
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
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
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
service = web
private dns = web
source = image
cpuLimit = - (default)
healthcheck = - (default)
image = "nginx:1.27-alpine" (default -)
maxRetries = 10 (default)
memLimit = - (default)
preDeployCommand = - (default)
privateDns = "web" (default)
registryCredential = - (default)
replicas = 1 (default)
restartPolicy = "unless-stopped" (default)
startCommand = - (default)
template = - (default)
updateOrder = - (default)
configs.sentry = "/etc/sentry"
domains = acme.com
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
  "domains": [
    {
      "action": {
        "records": [
          {
            "name": "@",
            "type": "AAAA",
            "value": "2001:db8::10"
          }
        ],
        "type": "dns"
      },
      "hostname": "acme.com",
      "kind": "custom",
      "port": null,
      "reason": "DNS points to another server",
      "records": [
        {
          "name": "@",
          "type": "AAAA",
          "value": "2001:db8::10"
        }
      ],
      "service": "web",
      "status": "needs_attention"
    }
  ],
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 26
  },
  "id": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
  "lineage": "e358ac6a-560a-468f-9f9f-43fc58547dfd",
  "name": "web",
  "private_dns": "web",
  "row": "e358ac6a-560a-468f-9f9f-43fc58547dfd:node",
  "settings": [
    {
      "apply": "staged",
      "default": null,
      "path": "web.cpuLimit",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.healthcheck",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.image",
      "value": "nginx:1.27-alpine"
    },
    {
      "apply": "staged",
      "default": 10,
      "path": "web.maxRetries",
      "value": 10
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.memLimit",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.preDeployCommand",
      "value": null
    },
    {
      "apply": "staged",
      "default": "web",
      "path": "web.privateDns",
      "value": "web"
    },
    {
      "apply": "immediate",
      "default": null,
      "path": "web.registryCredential",
      "value": null
    },
    {
      "apply": "staged",
      "default": 1,
      "path": "web.replicas",
      "value": 1
    },
    {
      "apply": "staged",
      "default": "unless-stopped",
      "path": "web.restartPolicy",
      "value": "unless-stopped"
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.startCommand",
      "value": null
    },
    {
      "apply": "immediate",
      "default": null,
      "path": "web.template",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.updateOrder",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.configs.sentry",
      "value": "/etc/sentry"
    }
  ],
  "source": "image",
  "template": null,
  "values": {
    "configs": {
      "sentry": "/etc/sentry"
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
Restarted shop-production/web on machine-1.
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
      "action": "restart",
      "container_id": "c8361accee66e39d0e17a6493a336223aa2a29f4efde6c6f4d9847bd226e2c72",
      "machine_id": "c109917d492d460a994dbf4b43162146",
      "service": "shop-production/web"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
Restarted shop-production/web on machine-1.
```

### service stop

```console
$ ployz service stop worker
# exit 0
```

stdout:
```
Stopped shop-production/worker on machine-1.
```

### ps with a stopped Service

```console
$ ployz ps
# exit 0
```

stdout:
```
SERVICE	KIND	SERVER	STATE
api	service	machine-1	running
postgres	service	machine-1	running
web	service	machine-1	running
worker	service	machine-1	exited with code 2
```

### service start

```console
$ ployz service start worker
# exit 0
```

stdout:
```
Started shop-production/worker on machine-1.
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
      "container_id": "7dfd628663aeea6f3b1c5e43b411d2830025a329502895be70a72061404a9b45",
      "machine_id": "c109917d492d460a994dbf4b43162146",
      "service": "shop-production/worker"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
Stopped shop-production/worker on machine-1.
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
      "container_id": "7dfd628663aeea6f3b1c5e43b411d2830025a329502895be70a72061404a9b45",
      "machine_id": "c109917d492d460a994dbf4b43162146",
      "service": "shop-production/worker"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
Started shop-production/worker on machine-1.
```

### service restart two Services

```console
$ ployz service restart api worker
# exit 0
```

stdout:
```
Restarted shop-production/api on machine-1.
Restarted shop-production/worker on machine-1.
```

## server

### server ls

```console
$ ployz server ls
# exit 0
```

stdout:
```
SERVER	MEMBERSHIP	STORAGE	PUBLIC IP	SUBNET	DAEMON	OS
machine-1	up	Managed volumes available (7.0 GB, 10 MB used, 7.0 GB free)	2001:db8::10	10.210.0.0/24	0.3.0-beta.6	Ubuntu 24.04.4 LTS x86_64
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
          "[2001:db8::10]:51820",
          "192.168.122.39:51820",
          "10.77.6.58:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "c109917d492d460a994dbf4b43162146",
        "labels": {},
        "name": "machine-1",
        "public_ip": "2001:db8::10",
        "public_key": "KXeXNAhNTeoeUvjPg+Elv3vCK5cgNtIgwMRVqzVQ5Ew=",
        "runtime": {
          "architecture": "x86_64",
          "capabilities": [
            "ployz.build.grant.inspect.v1",
            "ployz.build.grant.mint.v1",
            "ployz.build.v1",
            "ployz.certificates.policy.v1",
            "ployz.certificates.publish.v1",
            "ployz.container.ensure.v1",
            "ployz.container.exec.v1",
            "ployz.container.inspect.v1",
            "ployz.container.list.v1",
            "ployz.image.ingest.ensure.v1",
            "ployz.image.list.v1",
            "ployz.image.prune.v1",
            "ployz.image.pull-from-machine.v1",
            "ployz.image.pull.v1",
            "ployz.logs.history.v2",
            "ployz.logs.tail.v1",
            "ployz.machine.enroll.v1",
            "ployz.machine.forget.v1",
            "ployz.machine.inspect.v1",
            "ployz.machine.leave.v1",
            "ployz.machine.list.v1",
            "ployz.machine.register.v1",
            "ployz.machine.set-management-client.v1",
            "ployz.machine.set-push-target.v1",
            "ployz.machine.storage-observation.v1",
            "ployz.machine.update.v1",
            "ployz.machine.upgrade.request.v1",
            "ployz.metrics.history.v1",
            "ployz.network.select-endpoint.v1",
            "ployz.rpc.describe-contract.v1",
            "ployz.runtime.watch.v1",
            "ployz.serving.publish.v1",
            "ployz.serving.routes.v1",
            "ployz.storage.inspect.v1",
            "ployz.storage.prepare.v1",
            "ployz.volume.copy.floor.v1",
            "ployz.volume.copy.inspect.v1",
            "ployz.volume.copy.set.v1",
            "ployz.volume.list.v1",
            "ployz.volume.remove.v1",
            "ployz.volume.seed.v1",
            "ployz.volume.size.set.v1"
          ],
          "daemon_version": "0.3.0-beta.6",
          "docker_version": "29.7.0",
          "hostname": "box-node-bb4121c3027ee229",
          "kernel_version": "7.0.0-38-generic",
          "memory_total_bytes": 4101672960,
          "os_pretty_name": "Ubuntu 24.04.4 LTS",
          "running_builds": 0
        },
        "subnet": "10.210.0.0/24"
      },
      "membership": "up",
      "rtt": null,
      "selected_endpoint": null,
      "storage": {
        "free_bytes": 6969716736,
        "size_bytes": 6979321856,
        "state": "pool",
        "used_bytes": 9605120
      }
    }
  ]
}
```

### server show

```console
$ ployz server show machine-1
# exit 0
```

stdout:
```
server = machine-1
id = c109917d492d460a994dbf4b43162146
phase = participating
os = Ubuntu 24.04.4 LTS x86_64
kernel = 7.0.0-38-generic
hostname = box-node-bb4121c3027ee229
daemon = 0.3.0-beta.6
docker = 29.7.0
public ip = 2001:db8::10
subnet = 10.210.0.0/24
accepts = services, builds, ingress
labels = -
endpoints = [2001:db8::10]:51820, 192.168.122.39:51820, 10.77.6.58:51820, 203.0.113.10:51820
storage = Managed volumes available (7.0 GB, 10 MB used, 7.0 GB free)
cpu = 2 CPUs, load 0.11
memory = 3.1 GB available of 4.1 GB
docker disk = 38.8 GB free of 73.6 GB
containers = 5
management = cli-b0d2e09a28af41d780a40e97aed6, cloud
```

### server show --json

```console
$ ployz --json server show machine-1
# exit 0
```

stdout:
```
{
  "server": {
    "advertised_endpoints": [
      "[2001:db8::10]:51820",
      "192.168.122.39:51820",
      "10.77.6.58:51820",
      "203.0.113.10:51820"
    ],
    "id": "c109917d492d460a994dbf4b43162146",
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "[2001:db8::10]:51820",
        "192.168.122.39:51820",
        "10.77.6.58:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "c109917d492d460a994dbf4b43162146",
      "labels": {},
      "name": "machine-1",
      "public_ip": "2001:db8::10",
      "public_key": "KXeXNAhNTeoeUvjPg+Elv3vCK5cgNtIgwMRVqzVQ5Ew=",
      "runtime": {
        "architecture": "x86_64",
        "capabilities": [
          "ployz.build.grant.inspect.v1",
          "ployz.build.grant.mint.v1",
          "ployz.build.v1",
          "ployz.certificates.policy.v1",
          "ployz.certificates.publish.v1",
          "ployz.container.ensure.v1",
          "ployz.container.exec.v1",
          "ployz.container.inspect.v1",
          "ployz.container.list.v1",
          "ployz.image.ingest.ensure.v1",
          "ployz.image.list.v1",
          "ployz.image.prune.v1",
          "ployz.image.pull-from-machine.v1",
          "ployz.image.pull.v1",
          "ployz.logs.history.v2",
          "ployz.logs.tail.v1",
          "ployz.machine.enroll.v1",
          "ployz.machine.forget.v1",
          "ployz.machine.inspect.v1",
          "ployz.machine.leave.v1",
          "ployz.machine.list.v1",
          "ployz.machine.register.v1",
          "ployz.machine.set-management-client.v1",
          "ployz.machine.set-push-target.v1",
          "ployz.machine.storage-observation.v1",
          "ployz.machine.update.v1",
          "ployz.machine.upgrade.request.v1",
          "ployz.metrics.history.v1",
          "ployz.network.select-endpoint.v1",
          "ployz.rpc.describe-contract.v1",
          "ployz.runtime.watch.v1",
          "ployz.serving.publish.v1",
          "ployz.serving.routes.v1",
          "ployz.storage.inspect.v1",
          "ployz.storage.prepare.v1",
          "ployz.volume.copy.floor.v1",
          "ployz.volume.copy.inspect.v1",
          "ployz.volume.copy.set.v1",
          "ployz.volume.list.v1",
          "ployz.volume.remove.v1",
          "ployz.volume.seed.v1",
          "ployz.volume.size.set.v1"
        ],
        "daemon_version": "0.3.0-beta.6",
        "docker_version": "29.7.0",
        "hostname": "box-node-bb4121c3027ee229",
        "kernel_version": "7.0.0-38-generic",
        "memory_total_bytes": 4101672960,
        "os_pretty_name": "Ubuntu 24.04.4 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.0.0/24"
    },
    "management_clients": [
      "cli-b0d2e09a28af41d780a40e97aed6",
      "cloud"
    ],
    "phase": "participating",
    "public_key": "KXeXNAhNTeoeUvjPg+Elv3vCK5cgNtIgwMRVqzVQ5Ew=",
    "rtts": [],
    "storage": {
      "free_bytes": 6969716736,
      "size_bytes": 6979321856,
      "state": "pool",
      "used_bytes": 9605120
    },
    "store_version": {
      "30e020e172464a95a9e21f9a2a465226": 143
    },
    "telemetry": {
      "bridge": {
        "bridge_attached_endpoints": 5,
        "bridge_free_endpoints": 248,
        "bridge_usable_endpoints": 253
      },
      "host": {
        "cpu_count": 2,
        "docker_root_free_bytes": 38758916096,
        "docker_root_total_bytes": 73635184640,
        "load_average_milli": 110,
        "managed_containers": 5,
        "memory_available_bytes": 3060064256,
        "memory_total_bytes": 4101672960,
        "observed_at_unix_seconds": 1792515567
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
2026-10-20T16:55:44.617186+00:00 machine-1 ployz | ployzd[5398]:  INFO ingress config routes= https://acme.com=[10.210.0.4:8080]
2026-10-20T16:57:20.800617+00:00 machine-1 ployz | ployzd[5398]:  INFO ingress config routes= https://acme.com=[]
2026-10-20T16:57:31.013256+00:00 machine-1 ployz | ployzd[5398]:  INFO ingress config routes= https://acme.com=[10.210.0.4:8080]
2026-10-20T16:57:39.433521+00:00 machine-1 ployz | ployzd[5398]:  INFO ingress config routes= https://acme.com=[]
2026-10-20T16:57:49.653851+00:00 machine-1 ployz | ployzd[5398]:  INFO ingress config routes= https://acme.com=[10.210.0.4:8080]
```

### server logs --json

```console
$ ployz --json server logs --tail 2
# exit 0
```

stdout:
```
{"container":null,"container_id":null,"deployment":null,"hook":null,"level":"info","line":"systemd[1]: ployz.service: Got notification message from PID 30698, but reception only permitted for main PID 5398","machine":"machine-1","message":"systemd[1]: ployz.service: Got notification message from PID 30698, but reception only permitted for main PID 5398\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-20T16:59:31.139342+00:00","ts":"2026-10-20T16:59:31.139342000Z"}
{"container":null,"container_id":null,"deployment":null,"hook":null,"level":"info","line":"systemd[1]: ployz.service: Got notification message from PID 30738, but reception only permitted for main PID 5398","machine":"machine-1","message":"systemd[1]: ployz.service: Got notification message from PID 30738, but reception only permitted for main PID 5398\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-20T16:59:33.247128+00:00","ts":"2026-10-20T16:59:33.247128000Z"}
```

