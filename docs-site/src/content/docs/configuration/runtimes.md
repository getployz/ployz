---
title: "Docker and host runtime modes"
description: "How to choose between the Docker and host runtime targets and between user and system service modes when installing the Ployz daemon on Linux and macOS."
llms:
  summary: "How to choose between the Docker and host runtime targets and between user and system service modes when installing the Ployz daemon on Linux and macOS."
---
When you install the Ployz daemon, you choose how it manages its sidecars and where its control-plane services bind. These choices are captured in two flags: `--runtime` and `--service-mode`. Together they determine which processes run where, how they are supervised, and how the daemon survives a restart.

***

## Runtime target

The runtime target controls how the WireGuard mesh, NATS store, gateway, and DNS sidecars are run.

| Value    | Description                                                                      |
| -------- | -------------------------------------------------------------------------------- |
| `docker` | Sidecars run as Docker containers sharing a `ployz-networking` network namespace |
| `host`   | Sidecars run directly on the host as processes or system units                   |

## Service mode

The service mode controls how sidecars are supervised and where the control-plane binds.

| Value    | Description                                                                                        |
| -------- | -------------------------------------------------------------------------------------------------- |
| `user`   | Child-process or container supervision; control-plane binds on loopback (Docker) or overlay (host) |
| `system` | systemd manages the sidecar units; control-plane binds on the overlay address                      |

***

## Runtime matrix

The two axes combine into three supported configurations:

| Runtime target | Service mode | Meaning                                                                        |
| -------------- | ------------ | ------------------------------------------------------------------------------ |
| `docker`       | `user`       | Docker-backed mesh/store/sidecars with loopback control-plane binding          |
| `host`         | `user`       | Host-backed mesh/store, child-process sidecars, overlay control-plane binding  |
| `host`         | `system`     | Host-backed mesh/store, system-managed sidecars, overlay control-plane binding |

:::note
Docker runtime only supports `--service-mode user`. Passing `--service-mode system` with `--runtime docker` is an error.
:::

:::note
System mode requires systemd and is only supported on Linux. On macOS, `--service-mode system` is not available.
:::

***

## Platform defaults

The installer chooses defaults based on your platform and available tooling:

### macOS

  **Default runtime:** `docker`\
  **Default service mode:** `user`

  On macOS, the daemon runs on the host and everything else runs inside the Docker Desktop Linux VM. OrbStack or Docker Desktop must be installed and running before you install Ployz.

  ```bash
  # Default macOS install — no flags needed
  ployzctl daemon install --runtime docker --service-mode user
  ```

  :::caution
    `--runtime host` is not the default on macOS and requires care. Most operators on macOS should use the Docker runtime.
  :::

### Linux

  **Default runtime:** `host`\
  **Default service mode:** `system` (if systemd is available and you have sudo), otherwise `user`

  On Linux, the daemon runs directly on the host. System mode is preferred when available because sidecars survive daemon restarts cleanly as managed systemd units.

  ```bash
  # Linux with systemd and sudo — system mode
  ployzctl daemon install --runtime host --service-mode system

  # Linux without systemd or without sudo — user mode
  ployzctl daemon install --runtime host --service-mode user
  ```

***

## Docker runtime (docker / user)

In the Docker runtime, the daemon starts a `ployz-networking` container that owns the WireGuard interface (`wg0`). NATS, gateway, DNS, and all workload containers join that container's network namespace so they share the overlay interface without needing host networking.

The daemon itself runs on the host and bridges into the container network using `OverlayBridge` (a userspace WireGuard and smoltcp bridge). The control plane binds on loopback addresses on the host side.

**When to use:**

* macOS (required — there is no host WireGuard on macOS without additional tooling)
* Linux environments where host networking modification is restricted
* Development and CI where Docker is already available

**Requirements:**

* Docker (or OrbStack on macOS) installed and running
* The daemon must be able to start and manage containers

***

## Host runtime, user mode (host / user)

In host user mode, the daemon spawns sidecars as child processes bound to the host network. WireGuard is configured directly on the host using the kernel module. The control-plane services bind on the node's overlay address.

Child processes are supervised by the daemon. If the daemon exits, sidecars may be restarted on the next daemon start depending on whether they are adopted or recreated. The daemon always inspects what is already running before touching anything.

**When to use:**

* Linux nodes where you do not need systemd unit management
* Environments where you want simpler process supervision without system service registration

**Requirements:**

* Linux with WireGuard kernel module available
* Root or appropriate capabilities for network interface configuration (or sudo)

***

## Host runtime, system mode (host / system)

In host system mode, sidecars are registered as systemd units. The daemon installs and enables units for NATS, gateway, and DNS during `daemon install`. On subsequent starts, the daemon adopts running units whose identity matches and recreates any that have drifted.

System mode provides the strongest data-plane durability: sidecars keep running even if `ployzd` is not, and they are restarted by systemd if they crash.

**When to use:**

* Production Linux nodes managed by systemd
* Environments where you need the data plane (WireGuard, NATS, gateway, DNS) to survive daemon restarts or upgrades cleanly

**Requirements:**

* Linux with systemd
* Root or `sudo` — system service registration requires elevated privileges

```bash
# System mode install requires sudo if not running as root
sudo ployzctl daemon install --runtime host --service-mode system
```

***

## Daemon restart behavior

The daemon is designed to be disposable. Restarting or upgrading `ployzd` does not disrupt the data plane:

| Component           | On daemon restart                                                 |
| ------------------- | ----------------------------------------------------------------- |
| Workload containers | Never touched                                                     |
| Gateway             | Adopted if running and config matches; recreated on drift         |
| DNS                 | Adopted if running and config matches; recreated on drift         |
| NATS                | Adopted if running and parent netns unchanged; recreated on drift |
| WireGuard           | Adopted if healthy                                                |
| CLI RPC listeners   | Ephemeral — restarted with the daemon                             |

The daemon checks identity (config hash, parent container ID, rendered unit identity) before deciding to adopt or recreate. This means upgrades are safe to run without draining workloads first.

***

## Choosing a runtime

### I am on macOS

  Use `--runtime docker --service-mode user`. This is the default and the only supported configuration for macOS. Install and start OrbStack or Docker Desktop before running the installer.

### I am on Linux with systemd and sudo

  Use `--runtime host --service-mode system`. This is the default the installer selects automatically when systemd and sudo are available. Sidecars will be managed as systemd units.

### I am on Linux without systemd or without sudo

  Use `--runtime host --service-mode user`. The installer selects this automatically when system mode is not available. Sidecars run as child processes of the daemon.

### I want to use Docker on Linux

  Use `--runtime docker --service-mode user`. This is valid on Linux and works the same way as macOS, with sidecars running inside Docker containers. Note that `--service-mode system` is not supported with the Docker runtime.
