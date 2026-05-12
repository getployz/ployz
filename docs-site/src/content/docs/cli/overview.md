---
title: "ployzctl commands, flags, and output modes"
description: "Complete reference for the ployzctl operator CLI — commands, global flags, and output modes for driving a Ployz cluster from the terminal."
llms:
  summary: "Complete reference for the ployzctl operator CLI — commands, global flags, and output modes for driving a Ployz cluster from the terminal."
---
`ployzctl` is the primary operator interface for Ployz. It wraps `ployzd`, the control plane daemon, and forwards all commands to it over a local Unix socket. Every operation is an explicit command with visible preconditions, a bounded effect, and a structured result — no background reconcilers, no desired-state drift.

## How ployzctl works

`ployzctl` has one native subcommand (`daemon`) and forwards everything else to `ployzd`. When you run `ployzctl machine ls`, ployzctl locates the `ployzd` binary next to itself and invokes it with the remaining arguments. This means global flags and all subcommands documented here are handled by `ployzd`.

```bash
# Native ployzctl command
ployzctl daemon install --runtime docker --service-mode user

# Forwarded to ployzd
ployzctl machine ls
ployzctl deploy -f ployz.toml
ployzctl mesh status my-network
```

## Global flags

These flags apply to every command and must be placed before the subcommand.

- `--config` (`PATH`):

Path to the Ployz configuration file. Overrides the default location resolved from the data directory.

- `--data-dir` (`PATH`):

Override the data directory used by the daemon. Useful when running multiple daemon instances on the same host.

- `--socket` (`PATH`):

Override the Unix socket path used to reach the running daemon. Defaults to the path resolved from the config or data directory.

- `--json` (`boolean`):

Print the full daemon response as JSON. Suitable for scripting and automation. Conflicts with `--plain`.

- `--plain` (`boolean`):

Print compact human-readable text with fewer tokens and less formatting. Conflicts with `--json`.

- `-q / --quiet` (`boolean`):

Suppress success output where possible. Errors are still printed to stderr.

## Output modes

Ployz supports three output modes to fit different operator workflows.

### Default

  Formatted, human-readable output. Tables, labels, and status summaries. Best for interactive use.

  ```bash
  ployzctl machine ls
  ```

### --json

  Full structured daemon response as JSON. Every field from the API payload is included. Best for scripts, agents, and pipelines.

  ```bash
  ployzctl --json machine ls
  ```

### --plain

  Compact text output with reduced whitespace and fewer tokens. Useful when feeding output to tools or language models that should parse it inline.

  ```bash
  ployzctl --plain machine ls
  ```

:::note
`--json` and `--plain` are mutually exclusive. Passing both flags is an error.
:::

## Top-level commands

### [machine](/cli/machine)

  Add, remove, inspect, and lifecycle-manage cluster nodes. Includes invite tokens and storage promotion.

### [deploy](/cli/deploy)

  Apply deploy manifests or deploy a single service inline. Includes dry-run and preview modes.

### [branch](/cli/branch)

  Fork namespaces for PR environments. Control which resources are copied and which are fresh.

### [migrate](/cli/migrate)

  Move a workload and its persistent volumes to a different machine. Uses ZFS incremental send.

### [mesh](/cli/mesh)

  Manage WireGuard overlay networks. Create, join, start, stop, and inspect mesh networks. Also available as `network`.

### [image](/cli/image)

  Push and distribute container images to cluster nodes. Inspect image availability and operations.

### [build](/cli/build)

  Build container images locally using Dockerfile or Railpack. Inspect build operations.

### Additional commands

| Command          | Description                                                                                                 |
| ---------------- | ----------------------------------------------------------------------------------------------------------- |
| `run`            | Start the ployzd daemon process directly. Accepts `--runtime`, `--service-mode`, and `--zfs-transfer-port`. |
| `status`         | Show the current daemon and cluster status.                                                                 |
| `doctor`         | Run preflight checks and diagnose common configuration problems.                                            |
| `runtime stream` | Stream runtime events from the daemon.                                                                      |

## The daemon install command

`daemon install` is the one command handled natively by `ployzctl` rather than forwarded to `ployzd`. It installs or reconfigures the local daemon runtime.

```bash
ployzctl daemon install --runtime docker --service-mode user
ployzctl daemon install --runtime host --service-mode system
```

- `--runtime` (`docker | host`) required:

The container runtime target. `docker` uses the Docker engine; `host` runs workloads directly on the host.

- `--service-mode` (`user | system`):

Whether the daemon service is installed as a user-level or system-level service.

- `--install-manifest` (`PATH`):

Optional path to a custom install manifest file.

## Error handling

When a command fails, ployzctl prints a structured error to stderr and exits with a non-zero code:

* **Exit 1** — I/O error, serialization failure, daemon error, or transport failure (daemon not reachable).
* **Exit 2** — Usage error or configuration error.

If the daemon is unreachable, ployzctl prints the socket path it attempted to connect to:

```
error: connection refused
is ployzd running? (socket: /run/user/1000/ployz/ployzd.sock)
```
