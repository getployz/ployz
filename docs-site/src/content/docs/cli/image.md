---
title: "ployzctl image — manage container images"
description: "Push, distribute, and inspect container images across cluster nodes. Track image availability by digest and machine, and query image transfer operations."
llms:
  summary: "Push, distribute, and inspect container images across cluster nodes. Track image availability by digest and machine, and query image transfer operations."
---
The `image` command manages container images across the cluster. Ployz tracks image availability per machine and per digest, which means deploys can verify that the required image is present on every placement target before starting containers. You can push images from your local Docker daemon or distribute images already present on one node to others.

```bash
ployzctl image <subcommand> [flags]
```

## Push vs. distribute

Ployz provides two image transfer primitives:

* **`push`** — uploads an image from your local Docker daemon to one or more cluster machines. Use this when you have the image locally (e.g. after a local build or a `docker pull`).
* **`distribute`** — copies an image that is already present on one cluster node to other nodes, without involving the local machine. Use this after a `build local` or after a `push` to spread the image cluster-wide.

## Subcommands

### status — check image availability

  Show which images are available on which machines. Can be filtered by digest and/or machine.

  ```bash
  ployzctl image status [--digest <digest>] [--machine <machine>]
  ```

  - `--digest` (`string`):

    Filter results to a specific image digest, e.g. `sha256:abc123...`. If omitted, availability records for all known images are returned.
  

  - `--machine` (`string`):

    Filter results to a specific machine ID. If omitted, all machines are included.
  

  ```bash
  # Check availability of a specific image across all machines
  ployzctl --json image status --digest sha256:abc123

  # Check all images on a specific machine
  ployzctl image status --machine machine-a
  ```

### push — push an image to machines

  Push a container image from the local Docker daemon to one or more cluster machines. The image is transferred over the cluster's WireGuard overlay network.

  ```bash
  ployzctl image push <image> --to <machine-id> [--to <machine-id>...] [flags]
  ```

  - `image` (`string`) required:

    The local image reference to push, e.g. `myapp:latest` or `ghcr.io/myorg/app:v1.2.3`. The image must be present in the local Docker daemon.
  

  - `--to` (`machine-id`) required:

    Machine ID to push the image to. Can be specified multiple times to push to multiple machines simultaneously.
  

  - `--platform` (`string`):

    Target platform in `os/arch` format, e.g. `linux/amd64` or `linux/arm64`. If omitted, the daemon uses the image's native platform.
  

  - `--expected-digest` (`string`):

    Expected image digest. If the pushed image's digest does not match, the operation fails. Useful for ensuring you push exactly the image you intend.
  

  The response includes an operation ID, the image artifact details (digest, size, platform), and a result per target machine with status (`present`, `skipped_present`, or `failed`) and any error messages.

### distribute — distribute an image between cluster nodes

  Copy an image already present on one cluster node to other nodes. No data leaves the cluster — the source machine transfers directly to the targets over the overlay network.

  ```bash
  ployzctl image distribute --digest <digest> --from <machine> --to <machine-id> [--to <machine-id>...] [flags]
  ```

  - `--digest` (`string`) required:

    Digest of the image to distribute, e.g. `sha256:abc123...`. The image must already be present on the `--from` machine.
  

  - `--from` (`string`) required:

    Machine ID of the source machine that holds the image.
  

  - `--to` (`machine-id`) required:

    Machine ID to distribute the image to. Can be specified multiple times.
  

  - `--platform` (`string`):

    Target platform in `os/arch` format. If omitted, the image's native platform is used.
  

  The response includes the operation ID, source machine, digest, and per-target results.

### inspect — inspect image details

  Inspect detailed metadata for a specific image by digest. Queries the daemon for availability records and artifact details.

  ```bash
  ployzctl image inspect --digest <digest> [--reference <ref>] [--machine <machine>]
  ```

  - `--digest` (`string`) required:

    Image digest to inspect, e.g. `sha256:abc123...`.
  

  - `--reference` (`string`):

    Optional image reference (tag or name) associated with the digest, used for display and record enrichment.
  

  - `--machine` (`string`):

    Inspect the image on a specific machine. If omitted, availability is checked across all known machines.
  

### operation list — list image operations

  List all image transfer operations tracked by the daemon, including push and distribute operations.

  ```bash
  ployzctl image operation list
  ```

  Use `--json` to get full structured records for scripting or monitoring.

  ```bash
  ployzctl --json image operation list
  ```

### operation get — get a specific image operation

  Retrieve the details of a single image operation by its ID.

  ```bash
  ployzctl image operation get <id>
  ```

  - `id` (`string`) required:

    The operation ID returned when the push or distribute was initiated.
  

## Examples

```bash
# Check if an image is available across the cluster
ployzctl image status --digest sha256:abc123

# Push a local image to two machines
ployzctl image push myapp:latest \
--to machine-a \
--to machine-b

# Push with digest verification
ployzctl image push ghcr.io/myorg/app:v1.2.3 \
--to machine-a \
--expected-digest sha256:abc123

# Distribute an image from machine-a to the rest of the cluster
ployzctl image distribute \
--digest sha256:abc123 \
--from machine-a \
--to machine-b \
--to machine-c

# Inspect image details
ployzctl --json image inspect --digest sha256:abc123

# List all image operations
ployzctl image operation list

# Get details of a specific operation
ployzctl image operation get op-abc123
```

:::tip
After a `build local`, use `image distribute` to spread the built image to all placement targets before deploying. This avoids per-machine pull latency at deploy time.
:::
