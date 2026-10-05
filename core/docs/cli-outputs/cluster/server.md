### server add: found a Cluster (pipe)

```console
$ ployz server add --standalone --no-install root@172.19.0.2 --name alpha
# exit 0
```

stdout:
```
Switched context to 'default'
Initialised Server alpha (d7bb999f0d454dbcacc63d085021de34)
[+] Running service ingress 0/1
 • Container ingress on alpha  Pending
[+] Running service ingress 0/1
 … Container ingress on alpha  Running
[+] Running service ingress 0/1
 … Container ployz-create-ea1d2e036ece06b10ff008ec3e8d4eb4a014bfc2f760b3bceb664e231aeaff65 on alpha  waiting for health  0.0s
[+] Running service ingress 1/1
 ✔ Container ployz-create-ea1d2e036ece06b10ff008ec3e8d4eb4a014bfc2f760b3bceb664e231aeaff65 on alpha  Healthy
✓ Deployed to default
  1 ready · 1 created · 1 machine
```

stderr:
```
WARNING: Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
```

### server add: join a second Server (TTY)

```console
$ ployz server add --standalone --no-install root@172.19.0.3 --name beta
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Storage preparation [zfs/none] (none is Docker only, not recommended): WARNING: Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
Added Server beta (3ba1dcbd6b854ea699b76f6ef296576d)
Waiting for ssh://root@172.19.0.3 to participate: connection attempt failed: Server phase is joining; retrying for up to 299s. Check outbound firewall access if this connection is blocked.
# exit 0

```

### server add: join a third Server (--json)

```console
$ ployz server add --standalone --no-install root@172.19.0.4 --name gamma --json
# exit 0
```

stdout:
```
{
  "server": {
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "172.19.0.4:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "0523a6681d894d128f69ea28b7ed62d5",
      "labels": {},
      "name": "gamma",
      "public_ip": "203.0.113.10",
      "public_key": "Xxp4vxAgIulypSLB5IiNDhd4x4T69qEDJnKzCiwpbkY=",
      "runtime": {
        "architecture": "x86_64",
        "daemon_version": "0.2.3",
        "docker_version": "29.1.3",
        "hostname": "5cfa19714dbf",
        "kernel_version": "6.8.0-138-generic",
        "memory_total_bytes": 33649131520,
        "os_pretty_name": "Ubuntu 24.04.5 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.2.0/24"
    }
  },
  "warnings": [
    "Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive."
  ]
}
```

stderr:
```
WARNING: Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
Added Server gamma (0523a6681d894d128f69ea28b7ed62d5)
Waiting for ssh://root@172.19.0.4 to participate: connection attempt failed: Server phase is joining; retrying for up to 298s. Check outbound firewall access if this connection is blocked.
```

### server add: found a second, separate Cluster (rich ingress progress) (TTY)

```console
$ ployz server add --standalone --no-install root@172.19.0.5 --name solo --ployz-config /tmp/ployz-cap-home/solo.yaml
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Storage preparation [zfs/none] (none is Docker only, not recommended): WARNING: Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
Switched context to 'default'
Initialised Server solo (11b07efcce8e4822b59ca72d20433f1a)
Waiting for ssh://root@172.19.0.5 to participate: connection attempt failed: Server is not yet published in the Cluster store; retrying for up to 298s. Check outbound firewall access if this connection is blocked.
[+] Running service ingress 0/1
 • Container ingress on solo  Pending
[+] Running service ingress 0/1
 … Container ingress on solo  Running
[+] Running service ingress 0/1
 … Container ingress on solo  Running
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  Running
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  waiting for health  0.0s
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  waiting for health  1.0s
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  waiting for health  2.0s
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  waiting for health  3.0s
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  waiting for health  4.0s
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  waiting for health  5.0s
[+] Running service ingress 0/1
 … Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  waiting for health  5.0s
[+] Running service ingress 1/1
 ✔ Container ployz-create-86a1a036bca580635a80fea26c3310d748a16c3ee85dbabb61290021bd8f4f51 on solo  Healthy
✓ Deployed to default
  1 ready · 1 created · 1 machine
# exit 0

```

### server add: unreachable destination

```console
$ ployz server add --standalone --no-install root@10.255.255.1 --ssh-timeout 3
# exit 1
```

stderr:
```
WARNING: Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
all 1 connections from the explicit connection failed: connection attempt failed: SSH connection to root@10.255.255.1 timed out after 3 seconds; a slow or distant Server may need --ssh-timeout 60
```

### server add: without --standalone (needs Cloud login)

```console
$ ployz server add root@172.19.0.4
# exit 1
```

stderr:
```
not signed in to Cloud
next: ployz login
```

### server ls

```console
$ ployz server ls
# exit 0
```

stdout:
```
ID	NAME	MEMBERSHIP	STORAGE	SUBNET	GATEWAY	PUBLIC IP	ENDPOINTS	HOSTNAME	DAEMON	DOCKER	OS	KERNEL	ARCH
d7bb999f0d454dbcacc63d085021de34	alpha	up	Docker only	10.210.0.0/24	10.210.0.1	203.0.113.10	172.19.0.2:51820,203.0.113.10:51820	284d51ed2410	0.2.3	29.1.3	Ubuntu 24.04.5 LTS	6.8.0-138-generic	x86_64
3ba1dcbd6b854ea699b76f6ef296576d	beta	up	Docker only	10.210.1.0/24	10.210.1.1	203.0.113.10	172.19.0.3:51820,203.0.113.10:51820	037d3af757b3	0.2.3	29.1.3	Ubuntu 24.04.5 LTS	6.8.0-138-generic	x86_64
0523a6681d894d128f69ea28b7ed62d5	gamma	up	Docker only	10.210.2.0/24	10.210.2.1	203.0.113.10	172.19.0.4:51820,203.0.113.10:51820	5cfa19714dbf	0.2.3	29.1.3	Ubuntu 24.04.5 LTS	6.8.0-138-generic	x86_64
```

### server ls --json

```console
$ ployz server ls --json
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
          "172.19.0.2:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "d7bb999f0d454dbcacc63d085021de34",
        "labels": {},
        "name": "alpha",
        "public_ip": "203.0.113.10",
        "public_key": "XZYeqOGDcGr+dhj2HX53vN6PYb45cSU7+GWYyw1Tqhg=",
        "runtime": {
          "architecture": "x86_64",
          "daemon_version": "0.2.3",
          "docker_version": "29.1.3",
          "hostname": "284d51ed2410",
          "kernel_version": "6.8.0-138-generic",
          "memory_total_bytes": 33649131520,
          "os_pretty_name": "Ubuntu 24.04.5 LTS",
          "running_builds": 0
        },
        "subnet": "10.210.0.0/24"
      },
      "membership": "up",
      "rtt": null,
      "selected_endpoint": null,
      "storage": {
        "state": "stateless"
      }
    },
    {
      "gateway": "10.210.1.1",
      "machine": {
        "accepts_builds": true,
        "accepts_ingress": true,
        "accepts_services": true,
        "advertised_endpoints": [
          "172.19.0.3:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "3ba1dcbd6b854ea699b76f6ef296576d",
        "labels": {},
        "name": "beta",
        "public_ip": "203.0.113.10",
        "public_key": "hvumNXIGZh+QDRSl/ZES4V3SmdyO7G9OTtskLU1esTY=",
        "runtime": {
          "architecture": "x86_64",
          "daemon_version": "0.2.3",
          "docker_version": "29.1.3",
          "hostname": "037d3af757b3",
          "kernel_version": "6.8.0-138-generic",
          "memory_total_bytes": 33649131520,
          "os_pretty_name": "Ubuntu 24.04.5 LTS",
          "running_builds": 0
        },
        "subnet": "10.210.1.0/24"
      },
      "membership": "up",
      "rtt": null,
      "selected_endpoint": "172.19.0.3:51820",
      "storage": {
        "state": "stateless"
      }
    },
    {
      "gateway": "10.210.2.1",
      "machine": {
        "accepts_builds": true,
        "accepts_ingress": true,
        "accepts_services": true,
        "advertised_endpoints": [
          "172.19.0.4:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "0523a6681d894d128f69ea28b7ed62d5",
        "labels": {},
        "name": "gamma",
        "public_ip": "203.0.113.10",
        "public_key": "Xxp4vxAgIulypSLB5IiNDhd4x4T69qEDJnKzCiwpbkY=",
        "runtime": {
          "architecture": "x86_64",
          "daemon_version": "0.2.3",
          "docker_version": "29.1.3",
          "hostname": "5cfa19714dbf",
          "kernel_version": "6.8.0-138-generic",
          "memory_total_bytes": 33649131520,
          "os_pretty_name": "Ubuntu 24.04.5 LTS",
          "running_builds": 0
        },
        "subnet": "10.210.2.0/24"
      },
      "membership": "up",
      "rtt": null,
      "selected_endpoint": "172.19.0.4:51820",
      "storage": {
        "state": "stateless"
      }
    }
  ]
}
```

### server inspect

```console
$ ployz server inspect alpha
# exit 0
```

stdout:
```
{
  "server": {
    "advertised_endpoints": [
      "172.19.0.2:51820",
      "203.0.113.10:51820"
    ],
    "id": "d7bb999f0d454dbcacc63d085021de34",
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "172.19.0.2:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "d7bb999f0d454dbcacc63d085021de34",
      "labels": {},
      "name": "alpha",
      "public_ip": "203.0.113.10",
      "public_key": "XZYeqOGDcGr+dhj2HX53vN6PYb45cSU7+GWYyw1Tqhg=",
      "runtime": {
        "architecture": "x86_64",
        "daemon_version": "0.2.3",
        "docker_version": "29.1.3",
        "hostname": "284d51ed2410",
        "kernel_version": "6.8.0-138-generic",
        "memory_total_bytes": 33649131520,
        "os_pretty_name": "Ubuntu 24.04.5 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.0.0/24"
    },
    "management_clients": [],
    "phase": "participating",
    "public_key": "XZYeqOGDcGr+dhj2HX53vN6PYb45cSU7+GWYyw1Tqhg=",
    "rtts": [
      {
        "address": "[fdcc:5f1a:78bf:1020:22e9:72a5:22c1:e488]:7570",
        "machine": {
          "id": "0523a6681d894d128f69ea28b7ed62d5",
          "name": "gamma"
        },
        "peer_id": "26a2dca4-54a5-48a0-91d8-acf1afdedf9b",
        "statistics": {
          "median_ns": 0,
          "population_stddev_ns": 0
        }
      },
      {
        "address": "[fdcc:86fb:a635:7206:661f:900d:14a5:fd91]:7570",
        "machine": {
          "id": "3ba1dcbd6b854ea699b76f6ef296576d",
          "name": "beta"
        },
        "peer_id": "a6483bf3-e451-407e-961c-04b875d92b23",
        "statistics": {
          "median_ns": 0,
          "population_stddev_ns": 0
        }
      }
    ],
    "storage": {
      "state": "stateless"
    },
    "store_version": {
      "26a2dca454a548a091d8acf1afdedf9b": 1,
      "a6483bf3e451407e961c04b875d92b23": 1,
      "ce58aa53622f45e6a1f5219f50054827": 5
    },
    "telemetry": {
      "bridge": {
        "bridge_attached_endpoints": 1,
        "bridge_free_endpoints": 252,
        "bridge_usable_endpoints": 253
      },
      "host": {
        "cpu_count": 24,
        "docker_root_free_bytes": 256068272128,
        "docker_root_total_bytes": 778964144128,
        "load_average_milli": 1200,
        "managed_containers": 1,
        "memory_available_bytes": 18441940992,
        "memory_total_bytes": 33649131520,
        "observed_at_unix_seconds": 1791164346
      },
      "scope": "full"
    }
  },
  "upgrade": null
}
```

### server inspect --json

```console
$ ployz server inspect alpha --json
# exit 0
```

stdout:
```
{
  "server": {
    "advertised_endpoints": [
      "172.19.0.2:51820",
      "203.0.113.10:51820"
    ],
    "id": "d7bb999f0d454dbcacc63d085021de34",
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "172.19.0.2:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "d7bb999f0d454dbcacc63d085021de34",
      "labels": {},
      "name": "alpha",
      "public_ip": "203.0.113.10",
      "public_key": "XZYeqOGDcGr+dhj2HX53vN6PYb45cSU7+GWYyw1Tqhg=",
      "runtime": {
        "architecture": "x86_64",
        "daemon_version": "0.2.3",
        "docker_version": "29.1.3",
        "hostname": "284d51ed2410",
        "kernel_version": "6.8.0-138-generic",
        "memory_total_bytes": 33649131520,
        "os_pretty_name": "Ubuntu 24.04.5 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.0.0/24"
    },
    "management_clients": [],
    "phase": "participating",
    "public_key": "XZYeqOGDcGr+dhj2HX53vN6PYb45cSU7+GWYyw1Tqhg=",
    "rtts": [
      {
        "address": "[fdcc:5f1a:78bf:1020:22e9:72a5:22c1:e488]:7570",
        "machine": {
          "id": "0523a6681d894d128f69ea28b7ed62d5",
          "name": "gamma"
        },
        "peer_id": "26a2dca4-54a5-48a0-91d8-acf1afdedf9b",
        "statistics": {
          "median_ns": 0,
          "population_stddev_ns": 0
        }
      },
      {
        "address": "[fdcc:86fb:a635:7206:661f:900d:14a5:fd91]:7570",
        "machine": {
          "id": "3ba1dcbd6b854ea699b76f6ef296576d",
          "name": "beta"
        },
        "peer_id": "a6483bf3-e451-407e-961c-04b875d92b23",
        "statistics": {
          "median_ns": 0,
          "population_stddev_ns": 0
        }
      }
    ],
    "storage": {
      "state": "stateless"
    },
    "store_version": {
      "26a2dca454a548a091d8acf1afdedf9b": 1,
      "a6483bf3e451407e961c04b875d92b23": 1,
      "ce58aa53622f45e6a1f5219f50054827": 5
    },
    "telemetry": {
      "bridge": {
        "bridge_attached_endpoints": 1,
        "bridge_free_endpoints": 252,
        "bridge_usable_endpoints": 253
      },
      "host": {
        "cpu_count": 24,
        "docker_root_free_bytes": 256068268032,
        "docker_root_total_bytes": 778964144128,
        "load_average_milli": 1200,
        "managed_containers": 1,
        "memory_available_bytes": 18446667776,
        "memory_total_bytes": 33649131520,
        "observed_at_unix_seconds": 1791164346
      },
      "scope": "full"
    }
  },
  "upgrade": null
}
```

### server logs -n 5

```console
$ ployz server logs -n 5
# exit 0
```

stdout:
```
2026-10-05T01:39:06+00:00 alpha ployz | ployz testkit journal entry
2026-10-05T01:39:06+00:00 beta ployz | ployz testkit journal entry
2026-10-05T01:39:06+00:00 gamma ployz | ployz testkit journal entry
```

### server logs --json

```console
$ ployz server logs -n 5 --json | head -5
```

stdout (first lines):
```
{"container_id":null,"hook":null,"machine":"alpha","message":"ployz testkit journal entry\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-05T01:39:06+00:00"}
{"container_id":null,"hook":null,"machine":"beta","message":"ployz testkit journal entry\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-05T01:39:06+00:00"}
{"container_id":null,"hook":null,"machine":"gamma","message":"ployz testkit journal entry\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-05T01:39:06+00:00"}
```

### server clean (list orphan Namespaces)

```console
$ ployz server clean
# exit 0
```

stdout:
```
Every Namespace on the Servers that answered is in a Project.
```

### server clean --json

```console
$ ployz server clean --json
# exit 0
```

stdout:
```
{
  "failures": [],
  "namespaces": [],
  "next": null,
  "omitted": []
}
```

### server build-cache-clear

```console
$ ployz server build-cache-clear
# exit 1
```

stderr:
```
build resource cleanup could not be confirmed: retained builder state is quarantined; confirm builder ployz-1000 and its host processes have stopped before clearing /var/tmp/ployz-build-1000/ployz-1000.lock
```

### server build-cache-clear --json

```console
$ ployz server build-cache-clear --json
# exit 1
```

stdout:
```
{"error":{"code":"internal","details":null,"message":"build resource cleanup could not be confirmed: retained builder state is quarantined; confirm builder ployz-1000 and its host processes have stopped before clearing /var/tmp/ployz-build-1000/ployz-1000.lock"}}
```

### server set (labels)

```console
$ ployz server set gamma --label-add zone=b
# exit 0
```

stdout:
```
Updated Server gamma (0523a6681d894d128f69ea28b7ed62d5)
```

### server set --json

```console
$ ployz server set gamma --label-rm zone --json
# exit 0
```

stdout:
```
{
  "server": {
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "172.19.0.4:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "0523a6681d894d128f69ea28b7ed62d5",
      "labels": {},
      "name": "gamma",
      "public_ip": "203.0.113.10",
      "public_key": "Xxp4vxAgIulypSLB5IiNDhd4x4T69qEDJnKzCiwpbkY=",
      "runtime": {
        "architecture": "x86_64",
        "daemon_version": "0.2.3",
        "docker_version": "29.1.3",
        "hostname": "5cfa19714dbf",
        "kernel_version": "6.8.0-138-generic",
        "memory_total_bytes": 33649131520,
        "os_pretty_name": "Ubuntu 24.04.5 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.2.0/24"
    }
  }
}
```

stderr:
```
Updated Server gamma (0523a6681d894d128f69ea28b7ed62d5)
```

### server set (no change flags)

```console
$ ployz server set gamma
# exit 1
```

stderr:
```
at least one setting flag is required
```

### server upgrade (same version)

```console
$ ployz server upgrade 0.2.3 gamma
# exit 3
```

stdout:
```
Server gamma (0523a6681d894d128f69ea28b7ed62d5): upgrade 45fc6ae199a74f3cb73a440770647d95 failed at launching for 0.2.3: inspect Machine upgrade worker: No such file or directory (os error 2); inspect locally with `journalctl -u ployz-upgrade-45fc6ae199a74f3cb73a440770647d95.service`
Server gamma (0523a6681d894d128f69ea28b7ed62d5): upgrade 45fc6ae199a74f3cb73a440770647d95 failed at launching for 0.2.3: inspect Machine upgrade worker: No such file or directory (os error 2); inspect locally with `journalctl -u ployz-upgrade-45fc6ae199a74f3cb73a440770647d95.service`
```

stderr:
```
inspect Machine upgrade worker: No such file or directory (os error 2)
```

### server forget (standalone)

```console
$ ployz server forget
# exit 1
```

stderr:
```
not signed in to Cloud
next: ployz login
```

### server drain (live) (TTY)

```console
$ ployz server drain beta
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
Server beta no longer accepts Services.
Still on beta: ployz-system/ingress
Turning the services role back on does not move anything back.
# exit 0

```

### server drain --json (rerun)

```console
$ ployz server drain beta --json
# exit 0
```

stdout:
```
{
  "complete": true,
  "note": "Turning the services role back on does not move anything back.",
  "remaining": {
    "kind": "observed",
    "services": [
      "ployz-system/ingress"
    ],
    "unchosen": []
  },
  "server": {
    "accepts_builds": true,
    "accepts_ingress": true,
    "accepts_services": false,
    "advertised_endpoints": [
      "172.19.0.3:51820",
      "203.0.113.10:51820"
    ],
    "build_concurrency": null,
    "id": "3ba1dcbd6b854ea699b76f6ef296576d",
    "labels": {},
    "name": "beta",
    "public_ip": "203.0.113.10",
    "public_key": [
      134,
      251,
      166,
      53,
      114,
      6,
      102,
      31,
      144,
      13,
      20,
      165,
      253,
      145,
      18,
      225,
      93,
      210,
      153,
      220,
      142,
      236,
      111,
      78,
      78,
      219,
      36,
      45,
      77,
      94,
      177,
      54
    ],
    "runtime": {
      "architecture": "x86_64",
      "daemon_version": "0.2.3",
      "docker_version": "29.1.3",
      "hostname": "037d3af757b3",
      "kernel_version": "6.8.0-138-generic",
      "memory_total_bytes": 33649131520,
      "os_pretty_name": "Ubuntu 24.04.5 LTS",
      "running_builds": 0
    },
    "subnet": "10.210.1.0/24"
  },
  "services": [],
  "services_role": "already_off",
  "stopped": null
}
```

stderr:
```
Server beta already accepts no Services.
Still on beta: ployz-system/ingress
Turning the services role back on does not move anything back.
```

### server set --accepts-services=true

```console
$ ployz server set beta --accepts-services=true
# exit 0
```

stdout:
```
Updated Server beta (3ba1dcbd6b854ea699b76f6ef296576d)
```

### server rm --confirm

```console
$ ployz server rm gamma --confirm gamma
# exit 0
```

stdout:
```
Remove Server (0523a6681d894d128f69ea28b7ed62d5): gamma
Context: default
Based on what the connected machine can see; other machines may have additional resources.
Volumes the Cluster loses (0); only the Server's disk keeps their data:
Removed Server gamma (0523a6681d894d128f69ea28b7ed62d5) membership
```

stderr:
```
WARNING: Server gamma is running Services: ployz-system/ingress, shop-production/web. Move them off first: ployz server drain gamma
WARNING: Replicated Services may now be under-replicated: shop-production/web. Their replicas were not moved.
```

### server rm --json (unknown now)

```console
$ ployz server rm gamma --confirm gamma --json
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":null,"message":"Server gamma was not found"}}
```

### server ls after rm

```console
$ ployz server ls
# exit 0
```

stdout:
```
ID	NAME	MEMBERSHIP	STORAGE	SUBNET	GATEWAY	PUBLIC IP	ENDPOINTS	HOSTNAME	DAEMON	DOCKER	OS	KERNEL	ARCH
d7bb999f0d454dbcacc63d085021de34	alpha	up	Docker only	10.210.0.0/24	10.210.0.1	203.0.113.10	172.19.0.2:51820,203.0.113.10:51820	284d51ed2410	0.2.3	29.1.3	Ubuntu 24.04.5 LTS	6.8.0-138-generic	x86_64
3ba1dcbd6b854ea699b76f6ef296576d	beta	up	Docker only	10.210.1.0/24	10.210.1.1	203.0.113.10	172.19.0.3:51820,203.0.113.10:51820	037d3af757b3	0.2.3	29.1.3	Ubuntu 24.04.5 LTS	6.8.0-138-generic	x86_64
```

