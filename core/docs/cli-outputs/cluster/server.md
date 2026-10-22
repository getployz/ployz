### server add: found a Cluster (pipe)

```console
$ ployz server add --standalone --no-install root@172.26.0.2 --name alpha
# exit 0
```

stdout:
```
Initialised Server alpha.
```

stderr:
```
! Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
Switched context to default.
Waiting for ssh://root@172.26.0.2 to participate: connection attempt failed: Server is not yet published in the Cluster store; retrying for up to 295s. Check outbound firewall access if this connection is blocked.
Deploying ingress to default
  ingress (depth 0): waiting 0/1
  ingress (depth 0): booting 0/1
  ingress (depth 0): switching 1/1
  ingress (depth 0): switched 1/1
────────────────────────────
1 deployed · across 1 server
```

### server add: join a second Server (TTY)

```console
$ ployz server add --standalone --no-install root@172.26.0.3 --name beta
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
! Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
Added Server beta.
Waiting for ssh://root@172.26.0.3 to participate: Server 7f00d39ce93641bd8c5fdd7d91395aa7 isn't answering. Check that its Ployz daemon is running, then retry: the Server's daemon accepted the connection but didn't answer within 5s; it may still be starting; retrying for up to 294s. Check outbound firewall access if this connection is blocked.
Starting Globals on beta
✔ ingress on beta  running  11.8s
───────────────────────────
1 running · across 1 server
Server beta is ready.
# exit 0
```

### server add: join a third Server (--json)

```console
$ ployz server add --standalone --no-install root@172.26.0.4 --name gamma --json
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
        "172.26.0.4:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "7f571356a2ff4c36abb2cd9a719ec8ec",
      "labels": {},
      "name": "gamma",
      "public_ip": "203.0.113.10",
      "public_key": "5WYZW5+HmImuuYC26QJjzfUyodRpYlZjt9V2aJZInh0=",
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
          "ployz.sealing.generate.v1",
          "ployz.sealing.install.v1",
          "ployz.sealing.share.v1",
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
        "docker_version": "29.1.3",
        "hostname": "a47c8a8debb9",
        "kernel_version": "6.8.0-147-generic",
        "memory_total_bytes": 67421630464,
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
! Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
Added Server gamma.
Waiting for ssh://root@172.26.0.4 to participate: connection attempt failed: Server phase is joining; retrying for up to 298s. Check outbound firewall access if this connection is blocked.
Starting Globals on gamma
  ingress on gamma: pending
  ingress on gamma: creating
  ingress on gamma: running
───────────────────────────
1 running · across 1 server
Server gamma is ready.
```

### server add: found a second, separate Cluster (rich ingress progress) (TTY)

```console
$ ployz server add --standalone --no-install root@172.26.0.5 --name solo --ployz-config /tmp/ployz-cap-home/solo.yaml
```

TTY rendering (ANSI stripped; carriage-return redraws shown as separate lines; raw in .ansi):
```
! Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
Switched context to default.
Initialised Server solo.
Waiting for ssh://root@172.26.0.5 to participate: connection attempt failed: Server is not yet published in the Cluster store; retrying for up to 298s. Check outbound firewall access if this connection is blocked.
Deploying ingress to default
depth 0
  ✔ ingress  switched 1/1  6.9s
────────────────────────────
1 deployed · across 1 server
# exit 0
```

### server add: unreachable destination

```console
$ ployz server add --standalone --no-install root@10.255.255.1 --ssh-timeout 3
# exit 1
```

stderr:
```
! Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.
error: Ployz couldn't reach your Server's daemon.
  cause: SSH connection to root@10.255.255.1 timed out after 3 seconds; a slow or distant Server may need --ssh-timeout 60
```

### server add: without --standalone (needs Cloud login)

```console
$ ployz server add root@172.26.0.4
# exit 1
```

stderr:
```
error: No terminal to ask for the enrollment token. Pass it with --token-stdin, --token-file or PLOYZ_ENROLL_TOKEN.
next: ployz login
```

### server ls

```console
$ ployz server ls
# exit 0
```

stdout:
```
SERVER	MEMBERSHIP	STORAGE	PUBLIC IP	SUBNET	DAEMON	OS
alpha	up	Docker only	203.0.113.10	10.210.0.0/24	0.3.0-beta.6	Ubuntu 24.04.5 LTS x86_64
beta	up	Docker only	203.0.113.10	10.210.1.0/24	0.3.0-beta.6	Ubuntu 24.04.5 LTS x86_64
gamma	up	Docker only	203.0.113.10	10.210.2.0/24	0.3.0-beta.6	Ubuntu 24.04.5 LTS x86_64
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
          "172.26.0.2:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "91e8bbd81cd34c59a186d49d3375bbe8",
        "labels": {},
        "name": "alpha",
        "public_ip": "203.0.113.10",
        "public_key": "YRR/9dmEBep8qYrKzMwVlV5eWEUbM3990i514Aydrhg=",
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
            "ployz.sealing.generate.v1",
            "ployz.sealing.install.v1",
            "ployz.sealing.share.v1",
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
          "docker_version": "29.1.3",
          "hostname": "5a2028703ea3",
          "kernel_version": "6.8.0-147-generic",
          "memory_total_bytes": 67421630464,
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
          "172.26.0.3:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "7f00d39ce93641bd8c5fdd7d91395aa7",
        "labels": {},
        "name": "beta",
        "public_ip": "203.0.113.10",
        "public_key": "bXf7DxYwQD2pf3bcxJVY8z9kYVpBt8kVywgbLi93wWc=",
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
            "ployz.sealing.generate.v1",
            "ployz.sealing.install.v1",
            "ployz.sealing.share.v1",
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
          "docker_version": "29.1.3",
          "hostname": "b603bccc09c1",
          "kernel_version": "6.8.0-147-generic",
          "memory_total_bytes": 67421630464,
          "os_pretty_name": "Ubuntu 24.04.5 LTS",
          "running_builds": 0
        },
        "subnet": "10.210.1.0/24"
      },
      "membership": "up",
      "membership_evidence": {
        "kind": "up"
      },
      "rtt": null,
      "selected_endpoint": "172.26.0.3:51820",
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
          "172.26.0.4:51820",
          "203.0.113.10:51820"
        ],
        "build_concurrency": null,
        "id": "7f571356a2ff4c36abb2cd9a719ec8ec",
        "labels": {},
        "name": "gamma",
        "public_ip": "203.0.113.10",
        "public_key": "5WYZW5+HmImuuYC26QJjzfUyodRpYlZjt9V2aJZInh0=",
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
            "ployz.sealing.generate.v1",
            "ployz.sealing.install.v1",
            "ployz.sealing.share.v1",
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
          "docker_version": "29.1.3",
          "hostname": "a47c8a8debb9",
          "kernel_version": "6.8.0-147-generic",
          "memory_total_bytes": 67421630464,
          "os_pretty_name": "Ubuntu 24.04.5 LTS",
          "running_builds": 0
        },
        "subnet": "10.210.2.0/24"
      },
      "membership": "up",
      "membership_evidence": {
        "kind": "up"
      },
      "rtt": null,
      "selected_endpoint": "172.26.0.4:51820",
      "storage": {
        "state": "stateless"
      }
    }
  ]
}
```

### server show

```console
$ ployz server show alpha
# exit 0
```

stdout:
```
server = alpha
id = 91e8bbd81cd34c59a186d49d3375bbe8
phase = participating
os = Ubuntu 24.04.5 LTS x86_64
kernel = 6.8.0-147-generic
hostname = 5a2028703ea3
daemon = 0.3.0-beta.6
docker = 29.1.3
public ip = 203.0.113.10
subnet = 10.210.0.0/24
accepts = services, builds, ingress
labels = -
endpoints = 172.26.0.2:51820, 203.0.113.10:51820
storage = Docker only
cpu = 24 CPUs, load 34.94
memory = 41.8 GB available of 67.4 GB
docker disk = 839.2 GB free of 1975.1 GB
containers = 1
peer = beta 1.0 ms
peer = gamma 1.0 ms
management = -
```

### server show --json

```console
$ ployz server show alpha --json
# exit 0
```

stdout:
```
{
  "server": {
    "advertised_endpoints": [
      "172.26.0.2:51820",
      "203.0.113.10:51820"
    ],
    "id": "91e8bbd81cd34c59a186d49d3375bbe8",
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "172.26.0.2:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "91e8bbd81cd34c59a186d49d3375bbe8",
      "labels": {},
      "name": "alpha",
      "public_ip": "203.0.113.10",
      "public_key": "YRR/9dmEBep8qYrKzMwVlV5eWEUbM3990i514Aydrhg=",
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
          "ployz.sealing.generate.v1",
          "ployz.sealing.install.v1",
          "ployz.sealing.share.v1",
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
        "docker_version": "29.1.3",
        "hostname": "5a2028703ea3",
        "kernel_version": "6.8.0-147-generic",
        "memory_total_bytes": 67421630464,
        "os_pretty_name": "Ubuntu 24.04.5 LTS",
        "running_builds": 0
      },
      "subnet": "10.210.0.0/24"
    },
    "management_clients": [],
    "phase": "participating",
    "public_key": "YRR/9dmEBep8qYrKzMwVlV5eWEUbM3990i514Aydrhg=",
    "rtts": [
      {
        "address": "[fdcc:6d77:fb0f:1630:403d:a97f:76dc:c495]:7570",
        "machine": {
          "id": "7f00d39ce93641bd8c5fdd7d91395aa7",
          "name": "beta"
        },
        "peer_id": "1cd57189-5ff5-445f-a162-82c68d716a5a",
        "statistics": {
          "median_ns": 1000000,
          "population_stddev_ns": 0
        }
      },
      {
        "address": "[fdcc:e566:195b:9f87:9889:aeb9:80b6:e902]:7570",
        "machine": {
          "id": "7f571356a2ff4c36abb2cd9a719ec8ec",
          "name": "gamma"
        },
        "peer_id": "fc72cd1e-282a-4449-b33f-6e9c4c831303",
        "statistics": {
          "median_ns": 1000000,
          "population_stddev_ns": 349927
        }
      }
    ],
    "storage": {
      "state": "stateless"
    },
    "store_version": {
      "1cd571895ff5445fa16282c68d716a5a": 1,
      "a2619243e745420cb4b897e14695fb83": 5,
      "fc72cd1e282a4449b33f6e9c4c831303": 1
    },
    "telemetry": {
      "bridge": {
        "bridge_attached_endpoints": 1,
        "bridge_free_endpoints": 252,
        "bridge_usable_endpoints": 253
      },
      "host": {
        "cpu_count": 24,
        "docker_root_free_bytes": 839184707584,
        "docker_root_total_bytes": 1975103225856,
        "load_average_milli": 34940,
        "managed_containers": 1,
        "memory_available_bytes": 41683841024,
        "memory_total_bytes": 67421630464,
        "observed_at_unix_seconds": 1792512967
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
2026-10-20T16:16:07+00:00 alpha ployz | ployz testkit journal entry
2026-10-20T16:16:07+00:00 beta ployz | ployz testkit journal entry
2026-10-20T16:16:07+00:00 gamma ployz | ployz testkit journal entry
```

### server logs --json

```console
$ ployz server logs -n 5 --json | head -5
```

stdout (first lines):
```
{"container":null,"container_id":null,"deployment":null,"hook":null,"level":"info","line":"ployz testkit journal entry","machine":"alpha","message":"ployz testkit journal entry\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-20T16:16:07+00:00","ts":"2026-10-20T16:16:07.000000000Z"}
{"container":null,"container_id":null,"deployment":null,"hook":null,"level":"info","line":"ployz testkit journal entry","machine":"beta","message":"ployz testkit journal entry\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-20T16:16:07+00:00","ts":"2026-10-20T16:16:07.000000000Z"}
{"container":null,"container_id":null,"deployment":null,"hook":null,"level":"info","line":"ployz testkit journal entry","machine":"gamma","message":"ployz testkit journal entry\n","service":"ployz","service_id":null,"stream":"stdout","timestamp":"2026-10-20T16:16:07+00:00","ts":"2026-10-20T16:16:07.000000000Z"}
```

### server clean (list orphan Namespaces)

```console
$ ployz server clean
# exit 0
```

stdout:
```
NAMESPACE	SERVICES	VOLUMES
```

stderr:
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
# exit 0
```

stdout:
```
Cleared this host user's Ployz build cache.
```

### server build-cache-clear --json

```console
$ ployz server build-cache-clear --json
# exit 0
```

stdout:
```
{
  "build_cache": {
    "cleared": true
  }
}
```

### server set (labels)

```console
$ ployz server set gamma --label-add zone=b
# exit 0
```

stdout:
```
Updated Server gamma.
```

### server set --json

```console
$ ployz server set gamma --label-rm zone --json
# exit 0
```

stdout:
```
{
  "endpoint": null,
  "server": {
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [
        "172.26.0.4:51820",
        "203.0.113.10:51820"
      ],
      "build_concurrency": null,
      "id": "7f571356a2ff4c36abb2cd9a719ec8ec",
      "labels": {},
      "name": "gamma",
      "public_ip": "203.0.113.10",
      "public_key": "5WYZW5+HmImuuYC26QJjzfUyodRpYlZjt9V2aJZInh0=",
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
          "ployz.sealing.generate.v1",
          "ployz.sealing.install.v1",
          "ployz.sealing.share.v1",
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
        "docker_version": "29.1.3",
        "hostname": "a47c8a8debb9",
        "kernel_version": "6.8.0-147-generic",
        "memory_total_bytes": 67421630464,
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
Updated Server gamma.
```

### server set (no change flags)

```console
$ ployz server set gamma
# exit 2
```

stderr:
```
error: at least one setting flag is required
```

### server upgrade (fails at launch: the testkit has no systemd)

```console
$ ployz server upgrade 0.2.3 gamma
# exit 3
```

stdout:
```
Server gamma failed to upgrade to 0.2.3 while launching.
```

stderr:
```
error: Server gamma failed to upgrade to 0.2.3 while launching: could not inspect the Machine upgrade worker: No such file or directory (os error 2)
inspect: journalctl -u ployz-upgrade-9c1f32b7863745b1a6075a4e59261a69.service
```

### server upgrade --json (fails at launch)

```console
$ ployz server upgrade 0.2.3 gamma --json
# exit 3
```

stdout:
```
{
  "attempts": [
    {
      "attempt_id": "7f94c963cdcb491da8c38e4e077fa308",
      "error": "could not inspect the Machine upgrade worker: No such file or directory (os error 2)",
      "outcome": "failed",
      "server": "7f571356a2ff4c36abb2cd9a719ec8ec",
      "stage": "launching",
      "target": "0.2.3"
    }
  ],
  "follow_up_error": {
    "cause": [],
    "code": "internal",
    "details": {
      "inspect": [
        "journalctl -u ployz-upgrade-7f94c963cdcb491da8c38e4e077fa308.service"
      ]
    },
    "message": "Server gamma failed to upgrade to 0.2.3 while launching: could not inspect the Machine upgrade worker: No such file or directory (os error 2)"
  },
  "unattempted": []
}
```

stderr:
```
Server gamma failed to upgrade to 0.2.3 while launching.
error: Server gamma failed to upgrade to 0.2.3 while launching: could not inspect the Machine upgrade worker: No such file or directory (os error 2)
inspect: journalctl -u ployz-upgrade-7f94c963cdcb491da8c38e4e077fa308.service
```

### server forget (standalone)

```console
$ ployz server forget
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
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
      "172.26.0.3:51820",
      "203.0.113.10:51820"
    ],
    "build_concurrency": null,
    "id": "7f00d39ce93641bd8c5fdd7d91395aa7",
    "labels": {},
    "name": "beta",
    "public_ip": "203.0.113.10",
    "public_key": "bXf7DxYwQD2pf3bcxJVY8z9kYVpBt8kVywgbLi93wWc=",
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
        "ployz.sealing.generate.v1",
        "ployz.sealing.install.v1",
        "ployz.sealing.share.v1",
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
      "docker_version": "29.1.3",
      "hostname": "b603bccc09c1",
      "kernel_version": "6.8.0-147-generic",
      "memory_total_bytes": 67421630464,
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
```

### server set --accepts-services=true

```console
$ ployz server set beta --accepts-services=true
# exit 0
```

stdout:
```
Updated Server beta.
```

### server rm --confirm

```console
$ ployz server rm gamma --confirm gamma
# exit 0
```

stdout:
```
Removed Server gamma.
```

stderr:
```
! Server gamma is running Services: shop-production/web.
! Replicated Services may now be under-replicated: shop-production/web. Their replicas were not moved.
```

### server rm --json (unknown now)

```console
$ ployz server rm gamma --confirm gamma --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":null,"message":"Server gamma was not found"}}
```

### server ls after rm

```console
$ ployz server ls
# exit 0
```

stdout:
```
SERVER	MEMBERSHIP	STORAGE	PUBLIC IP	SUBNET	DAEMON	OS
alpha	up	Docker only	203.0.113.10	10.210.0.0/24	0.3.0-beta.6	Ubuntu 24.04.5 LTS x86_64
beta	up	Docker only	203.0.113.10	10.210.1.0/24	0.3.0-beta.6	Ubuntu 24.04.5 LTS x86_64
```

