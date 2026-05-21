---
title: "Daemon configuration reference"
description: "Complete reference for the ployzd TOML configuration file, including all fields, their defaults, and the environment variables that override them."
llms:
  summary: "Complete reference for the ployzd TOML configuration file, including all fields, their defaults, and the environment variables that override them."
---
The Ployz daemon reads its configuration from a TOML file at a platform-specific default path. Every field has a compiled-in default, so an empty or absent config file is valid — the daemon starts with sensible values and you only need to add fields you want to change.

## Config file location

### Linux

  ```
  ~/.config/ployz/config.toml
  ```

  When running as root, the daemon uses `/var/lib/ployz` for data storage and `/run/ployz/ployzd.sock` for the Unix socket, but the config file path follows the same XDG convention regardless of user.

### macOS

  ```
  ~/Library/Application Support/ployz/config.toml
  ```

  On macOS the daemon runs on the host. Everything else (NATS, gateway, DNS, workload containers) runs inside the Docker Desktop VM.

### Overriding the config path

Pass `--config PATH` to any `ployzd` or `ployzctl` command, or set the `PLOYZ_CONFIG` environment variable. The CLI flag takes precedence over the environment variable.

```bash
ployzctl status --config /etc/ployz/config.toml
PLOYZ_CONFIG=/etc/ployz/config.toml ployzctl status
```

***

## Top-level fields

- `data_dir` (`string`) required:

Absolute path to the directory where Ployz stores durable cluster state: network records, volume datasets, install metadata, and sidecar identity files.

Defaults to `~/.local/share/ployz` on Linux (non-root), `/var/lib/ployz` on Linux (root), and `~/Library/Application Support/ployz` on macOS.

- `socket` (`string`) required:

Path to the Unix domain socket used for CLI-to-daemon communication. `ployzctl` connects to this socket by default.

Defaults to `$XDG_RUNTIME_DIR/ployz/ployzd.sock` on Linux (non-root), `/run/ployz/ployzd.sock` on Linux (root), and `$TMPDIR/ployz/ployzd.sock` on macOS.

- `region` (`string`):

Logical region label for this node. Used for placement awareness and diagnostics. Has no effect unless your deploy manifests reference region constraints.

Also settable via `PLOYZ_REGION`.

- `az` (`string`):

Logical availability zone label for this node. Works alongside `region` for topology-aware placement.

Also settable via `PLOYZ_AZ`.

- `cluster_cidr` (`string`):

The IP address range from which node overlay subnets are carved. Each node in the mesh receives a `/subnet_prefix_len` subnet from this block. All nodes in the same mesh must share the same `cluster_cidr`.

:::caution
  Changing `cluster_cidr` after a mesh has been initialized requires destroying and re-creating the mesh. Coordinate this change across all nodes before applying it.
:::

- `subnet_prefix_len` (`number`):

The prefix length of the per-node subnet carved from `cluster_cidr`. With the default `/16` CIDR and a prefix length of `24`, each node gets a `/24` subnet — 254 usable addresses — and the cluster supports up to 256 nodes.

- `zfs_transfer_port` (`number`):

TCP port that the daemon listens on for incoming ZFS volume stream transfers during live migration. Both the sending and receiving node must be able to reach this port on the destination.

Also settable via `PLOYZ_ZFS_TRANSFER_PORT`, or with the `--zfs-transfer-port` CLI flag on `ployzd run`.

- `builtin_images_manifest` (`string`):

Path to a TOML manifest that overrides the built-in sidecar images (NATS, gateway, DNS). Leave unset to use the compiled-in image references.

Also settable via `PLOYZ_BUILTIN_IMAGES_MANIFEST`.

***

## Storage

- `storage.zfs_root` (`string`):

ZFS dataset path to use as the root for Ployz-managed volumes (for example, `tank/ployz`). When set, the daemon creates volume datasets under this root instead of using loopback-backed images. Requires ZFS to be installed and the dataset to already exist.

Only configurable via `config.toml` — there is no environment variable override for this field.

- `storage.overcommit_ratio` (`number`):

Ratio of allocated volume quota to the actual backing storage capacity that the daemon will allow. `1.0` means no overcommit: the sum of all volume quotas cannot exceed the pool's available space. Set to `1.5` to allow 50% overcommit.

Only configurable via `config.toml`.

***

## Gateway

- `gateway_listen_addr` (`string`):

Address and port the HTTP gateway listens on. To bind on a specific interface, replace `0.0.0.0` with the interface IP.

- `gateway_https_listen_addr` (`string`):

Address and port the HTTPS gateway listens on. When set, the gateway also serves TLS. Certificates are sourced from the cluster's routing store (SNI-based) or from the static paths configured in the gateway process environment.

Leave unset to serve HTTP only.

- `gateway_threads` (`number`):

Number of worker threads the gateway process uses to handle requests. Increase this on nodes that serve heavy HTTP traffic.

***

## Metrics

All three metrics addresses are unset by default. Set any of them to expose a Prometheus-compatible `/metrics` endpoint on that address.

- `daemon_metrics_listen_addr` (`string`):

Address the daemon's own metrics server listens on (for example, `127.0.0.1:9100`).

Also settable via `PLOYZ_DAEMON_METRICS_LISTEN_ADDR`.

- `dns_metrics_listen_addr` (`string`):

Address the DNS sidecar's metrics server listens on (for example, `127.0.0.1:9101`).

Also settable via `PLOYZ_DNS_METRICS_LISTEN_ADDR`.

- `gateway_metrics_listen_addr` (`string`):

Address the gateway sidecar's metrics server listens on (for example, `127.0.0.1:9102`).

Also settable via `PLOYZ_GATEWAY_METRICS_LISTEN_ADDR`.

***

## Environment variable overrides

Environment variables with the `PLOYZ_` prefix are merged after the TOML file, so they take precedence over file-based values. Variables that accept the same values as the TOML field use the same format.

| Variable                            | Overrides field                                                |
| ----------------------------------- | -------------------------------------------------------------- |
| `PLOYZ_CONFIG`                      | Config file path (not a field — controls which file is loaded) |
| `PLOYZ_REGION`                      | `region`                                                       |
| `PLOYZ_AZ`                          | `az`                                                           |
| `PLOYZ_ZFS_TRANSFER_PORT`           | `zfs_transfer_port`                                            |
| `PLOYZ_DAEMON_METRICS_LISTEN_ADDR`  | `daemon_metrics_listen_addr`                                   |
| `PLOYZ_DNS_METRICS_LISTEN_ADDR`     | `dns_metrics_listen_addr`                                      |
| `PLOYZ_GATEWAY_METRICS_LISTEN_ADDR` | `gateway_metrics_listen_addr`                                  |
| `PLOYZ_BUILTIN_IMAGES_MANIFEST`     | `builtin_images_manifest`                                      |

:::note
`storage.zfs_root` and `storage.overcommit_ratio` do not have environment variable overrides. Configure them only in `config.toml`.
:::

***

## Sample config.toml

The following example shows a node in a multi-region cluster with ZFS-backed volumes, HTTPS enabled on the gateway, and Prometheus metrics exposed on loopback.

```toml
# ~/.config/ployz/config.toml

# Topology labels used for placement decisions
region = "eu-west"
az     = "hel1-a"

# Overlay network settings
cluster_cidr      = "10.101.0.0/16"
subnet_prefix_len = 24

# ZFS volume transfer
zfs_transfer_port = 4319

# Gateway
gateway_listen_addr       = "0.0.0.0:80"
gateway_https_listen_addr = "0.0.0.0:443"
gateway_threads           = 4

# Prometheus metrics (loopback only)
daemon_metrics_listen_addr  = "127.0.0.1:9100"
dns_metrics_listen_addr     = "127.0.0.1:9101"
gateway_metrics_listen_addr = "127.0.0.1:9102"

# ZFS-backed persistent volumes
[storage]
zfs_root         = "tank/ployz"
overcommit_ratio = 1.0
```

:::tip
Run `ployzctl doctor` after changing config to verify the daemon can reach all expected resources with the new settings.
:::
