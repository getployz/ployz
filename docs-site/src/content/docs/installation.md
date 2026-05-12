---
title: "Install Ployz on Linux and macOS with any install method"
description: "Use the one-line installer, a manual release download, or a source build. Covers platform file paths, daemon registration, and environment variables."
llms:
  summary: "Use the one-line installer, a manual release download, or a source build. Covers platform file paths, daemon registration, and environment variables."
---
Ployz ships as a set of static binaries: `ployzctl` (the operator CLI), `ployzd` (the daemon), `ployz-gateway`, `ployz-dns`, and `nats-server`. You can install them in three ways: the one-line installer handles everything automatically, a manual release download lets you manage placement yourself, and a source build is available if you need a custom version.

### Linux

  On Linux, Ployz defaults to the **host** runtime — the daemon manages WireGuard, NATS, the gateway, DNS, and workload containers directly on the host. The service mode defaults to **system** if `systemctl` is available and you have `sudo` access, otherwise **user**.

  Supported architectures: `x86_64`, `aarch64`.

### macOS

  On macOS, Ployz defaults to the **Docker** runtime — everything except the daemon itself runs inside Docker Desktop's Linux VM. The service mode is always **user** on macOS (a LaunchAgent).

  Docker Desktop or OrbStack must be installed and running before you install or start Ployz.

  Supported architectures: `x86_64` (Intel), `aarch64` (Apple Silicon).

## One-line installer

The installer script at `https://ployz.sh` is the recommended way to install Ployz. It detects your platform, downloads the correct release payload, installs binaries to `~/.local/bin`, and registers the daemon as a service.

```bash
curl -fsSL https://ployz.sh | bash
```

To pin a specific version instead of installing latest:

```bash
curl -fsSL https://ployz.sh | bash -s -- --version v0.4.0
```

### Installer options

| Flag                  | Default                                           | Description                                    |
| --------------------- | ------------------------------------------------- | ---------------------------------------------- |
| `--runtime TARGET`    | `docker` on macOS, `host` on Linux                | Container runtime: `docker` or `host`          |
| `--service-mode MODE` | `system` if systemd + sudo available, else `user` | Service manager mode: `user` or `system`       |
| `--source SOURCE`     | `release`                                         | Install source: `release`, `git`, or `payload` |
| `--version VERSION`   | `latest`                                          | Release version to install, e.g. `v0.4.0`      |
| `--no-daemon-install` | —                                                 | Skip daemon service registration               |

:::note
The Docker runtime only works with `--service-mode user`. The system service mode requires Linux and the host runtime.
:::

### After install

Add `~/.local/bin` to your `PATH` if it is not already there:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

Add this line to your shell profile (`~/.bashrc`, `~/.zshrc`, etc.) to make it permanent. Then verify the installation:

```bash
ployzctl status
```

### Check installation status

Run the installer in probe mode to see a JSON summary of what is installed and where:

```bash
ployz.sh probe --json
```

The output includes the installed version, runtime target, service mode, config path, data directory, and socket path.

## Manual install from a release

Download the release payload for your platform directly and extract it yourself.

### Linux x86_64

  ```bash
  curl -fsSL https://ployz.sh/releases/latest/download/ployz-payload-linux-x86_64.tar.gz \
    -o ployz-payload.tar.gz
  tar -xzf ployz-payload.tar.gz
  install -d ~/.local/bin
  install -m 0755 payload/bin/ployzctl ~/.local/bin/ployzctl
  install -m 0755 payload/bin/ployzd ~/.local/bin/ployzd
  install -m 0755 payload/bin/ployz-gateway ~/.local/bin/ployz-gateway
  install -m 0755 payload/bin/ployz-dns ~/.local/bin/ployz-dns
  install -m 0755 payload/bin/nats-server ~/.local/bin/nats-server
  install -m 0755 payload/ployz.sh ~/.local/bin/ployz.sh
  ```

### Linux aarch64

  ```bash
  curl -fsSL https://ployz.sh/releases/latest/download/ployz-payload-linux-aarch64.tar.gz \
    -o ployz-payload.tar.gz
  tar -xzf ployz-payload.tar.gz
  install -d ~/.local/bin
  install -m 0755 payload/bin/ployzctl ~/.local/bin/ployzctl
  install -m 0755 payload/bin/ployzd ~/.local/bin/ployzd
  install -m 0755 payload/bin/ployz-gateway ~/.local/bin/ployz-gateway
  install -m 0755 payload/bin/ployz-dns ~/.local/bin/ployz-dns
  install -m 0755 payload/bin/nats-server ~/.local/bin/nats-server
  install -m 0755 payload/ployz.sh ~/.local/bin/ployz.sh
  ```

### macOS

  ```bash
  curl -fsSL https://ployz.sh/releases/latest/download/ployz-payload-darwin-aarch64.tar.gz \
    -o ployz-payload.tar.gz
  tar -xzf ployz-payload.tar.gz
  install -d ~/.local/bin
  install -m 0755 payload/bin/ployzctl ~/.local/bin/ployzctl
  install -m 0755 payload/bin/ployzd ~/.local/bin/ployzd
  install -m 0755 payload/bin/ployz-gateway ~/.local/bin/ployz-gateway
  install -m 0755 payload/bin/ployz-dns ~/.local/bin/ployz-dns
  install -m 0755 payload/bin/nats-server ~/.local/bin/nats-server
  install -m 0755 payload/ployz.sh ~/.local/bin/ployz.sh
  ```

  Use `ployz-payload-darwin-x86_64.tar.gz` for Intel Macs.

After extracting and placing the binaries, register the daemon manually:

```bash
ployzctl daemon install --runtime docker --service-mode user
```

Adjust `--runtime` and `--service-mode` for your platform. See [Daemon install](#daemon-install) below for all options.

## Build from source

Building from source requires Rust (stable toolchain) and `just`.

```bash
git clone https://github.com/getployz/ployz.git
cd ployz
just install
```

`just install` builds release binaries and installs them to `/usr/local/bin` by default. Pass a prefix to change the install location:

```bash
just install prefix="$HOME/.local"
```

:::caution
On Linux, the release build enables eBPF features that require kernel headers and `clang`. The `just build-release` step runs `./scripts/install-ebpf-bytecode.sh` automatically. If you are building in a minimal environment, ensure these dependencies are present.
:::

After building, register the daemon:

```bash
ployzctl daemon install --runtime host --service-mode user
```

## Daemon install

The daemon install step configures the service supervisor (systemd or launchd), writes the client configuration file, and starts `ployzd`.

```bash
ployzctl daemon install --runtime <docker|host> --service-mode <user|system>
```

### Linux — user service

  Installs a systemd user unit at `~/.config/systemd/user/ployzd.service` and enables it to start at login.

  ```bash
  ployzctl daemon install --runtime host --service-mode user
  ```

  Output:

  ```text
  daemon install complete
    runtime: host
    service-mode: user
    backend: systemd-user
    config: /home/alice/.config/ployz/config.toml
    socket: /run/user/1000/ployz/ployzd.sock
  ```

### Linux — system service

  Copies binaries to `/usr/local/bin`, installs a systemd system unit at `/etc/systemd/system/ployzd.service`, and enables it. Requires `sudo`.

  ```bash
  sudo ployzctl daemon install --runtime host --service-mode system
  ```

  Output:

  ```text
  daemon install complete
    runtime: host
    service-mode: system
    backend: systemd-system
    config: /home/alice/.config/ployz/config.toml
    socket: /run/ployz/ployzd.sock
  ```

### macOS

  Installs a LaunchAgent plist at `~/Library/LaunchAgents/dev.ployz.ployzd.plist` and bootstraps it into the user session immediately.

  ```bash
  ployzctl daemon install --runtime docker --service-mode user
  ```

  Output:

  ```text
  daemon install complete
    runtime: docker
    service-mode: user
    backend: launch-agent
    config: /Users/alice/Library/Application Support/ployz/config.toml
    socket: /var/folders/.../ployz/ployzd.sock
  ```

## File paths

Ployz follows platform conventions for config, data, and socket paths.

### Linux (non-root)

  | File             | Default path                                                                  |
  | ---------------- | ----------------------------------------------------------------------------- |
  | Config           | `~/.config/ployz/config.toml`                                                 |
  | Data directory   | `~/.local/share/ployz`                                                        |
  | Socket           | `$XDG_RUNTIME_DIR/ployz/ployzd.sock` (falls back to `/tmp/ployz/ployzd.sock`) |
  | Install manifest | `~/.local/share/ployz/install/manifest.env`                                   |
  | Binaries         | `~/.local/bin/`                                                               |

### Linux (root)

  | File             | Default path                                         |
  | ---------------- | ---------------------------------------------------- |
  | Config           | `~/.config/ployz/config.toml` (of the invoking user) |
  | Data directory   | `/var/lib/ployz`                                     |
  | Socket           | `/run/ployz/ployzd.sock`                             |
  | Install manifest | `/var/lib/ployz/install/manifest.env`                |
  | Binaries         | `/usr/local/bin/` (system install)                   |

### macOS

  | File             | Default path                                               |
  | ---------------- | ---------------------------------------------------------- |
  | Config           | `~/Library/Application Support/ployz/config.toml`          |
  | Data directory   | `~/Library/Application Support/ployz`                      |
  | Socket           | `$TMPDIR/ployz/ployzd.sock`                                |
  | Install manifest | `~/Library/Application Support/ployz/install/manifest.env` |
  | Binaries         | `~/.local/bin/`                                            |

Override the config path by setting `PLOYZ_CONFIG` or by passing `--config` to any command.

## Environment variables

All environment variables are prefixed with `PLOYZ_` and override the corresponding config file value.

| Variable                  | Description                                                                 |
| ------------------------- | --------------------------------------------------------------------------- |
| `PLOYZ_CONFIG`            | Path to the config file. Overrides the default platform path.               |
| `PLOYZ_REGION`            | Region label for this node, e.g. `eu-primary`. Used for placement topology. |
| `PLOYZ_AZ`                | Availability zone label, e.g. `hel1-a`.                                     |
| `PLOYZ_ZFS_TRANSFER_PORT` | Port used for ZFS incremental send during migration. Default: `4319`.       |

:::tip
Environment variables are the recommended way to set node-specific topology labels (`PLOYZ_REGION`, `PLOYZ_AZ`) in systemd unit files or launch configurations, so the same config file can be shared across machines in a cluster.
:::

## Configuration file

The config file is TOML. Most values have sensible defaults and do not need to be set explicitly. A minimal config on Linux with ZFS storage might look like:

```toml
[storage]
zfs_root = "tank/ployz"
```

Key config fields:

| Field                       | Default          | Description                                      |
| --------------------------- | ---------------- | ------------------------------------------------ |
| `data_dir`                  | Platform default | Persistent data directory                        |
| `socket`                    | Platform default | Unix socket path for CLI ↔ daemon communication  |
| `region`                    | —                | Region label for this node                       |
| `az`                        | —                | Availability zone label                          |
| `zfs_transfer_port`         | `4319`           | Port for ZFS state transfer during migration     |
| `gateway_listen_addr`       | `0.0.0.0:80`     | Address the gateway listens on for HTTP traffic  |
| `gateway_https_listen_addr` | —                | Address for HTTPS traffic (if TLS is configured) |
| `storage.zfs_root`          | —                | ZFS dataset root, e.g. `tank/ployz`              |
| `storage.overcommit_ratio`  | `1.0`            | Storage overcommit ratio                         |

## Verify the installation

After completing any of the install paths above, run the following to confirm the daemon is running and reachable:

```bash
ployzctl status
```

For a full JSON report of the installation state — runtime, paths, service backend, and version:

```bash
ployz.sh probe --json
```

### [Quickstart](/quickstart)

  Go from installed to a running cluster with a deployed workload.

### [What is Ployz?](/introduction)

  Understand what Ployz is and how it compares to Kubernetes.
