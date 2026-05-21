---
title: "ployzctl build — build container images"
description: "Build container images locally using Dockerfile or Railpack automatic detection. Inspect active and completed build operations by ID or list."
llms:
  summary: "Build container images locally using Dockerfile or Railpack automatic detection. Inspect active and completed build operations by ID or list."
---
The `build` command builds container images on the local machine and registers them with the cluster. Builds run on the machine where you invoke `ployzctl`, using either a standard Dockerfile or Railpack for automatic language detection. The resulting image is available for distribution to cluster nodes and subsequent deploys.

```bash
ployzctl build <subcommand> [flags]
```

## Build methods

Ployz supports two build methods:

| Method       | Description                                                                                                                                                  |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `dockerfile` | Standard Docker image build. The daemon uses the `Dockerfile` in the build context directory.                                                                |
| `railpack`   | Automatic language detection and build plan generation. No `Dockerfile` required — Railpack inspects the project and determines the appropriate build steps. |

:::tip
Use `railpack` for standard application stacks (Node.js, Python, Ruby, Go, etc.) when you do not want to maintain a Dockerfile. Use `dockerfile` when you need precise control over the build.
:::

## Subcommands

### local — build an image locally

  Build a container image from a local directory and register it with the daemon. The context directory is the root of the files sent to the builder.

  ```bash
  ployzctl build local --method <method> --image <image> [--platform <platform>] <context>
  ```

  **Positional arguments**

  - `context` (`PATH`) required:

    Path to the build context directory. All files in this directory are sent to the builder. For `dockerfile` builds, this directory must contain a `Dockerfile`.
  

  **Required flags**

  - `--method` (`dockerfile | railpack`) required:

    The build method to use. `dockerfile` uses the `Dockerfile` in the context; `railpack` auto-detects the build plan.
  

  - `--image` (`IMAGE`) required:

    The name and optional tag to assign to the built image, e.g. `myapp:latest` or `ghcr.io/myorg/app:v1.2.3`. This name is used for distribution and deploy references.
  

  **Optional flags**

  - `--platform` (`string`):

    Target platform in `os/arch` format, e.g. `linux/amd64` or `linux/arm64`. If omitted, the builder uses the host platform.
  

  The response includes an operation ID, the built image artifact (digest, size, platform), and an availability record if the image was registered locally.

  ```bash
  # Build using Dockerfile
  ployzctl build local --method dockerfile --image myapp:latest ./

  # Build using Railpack for a Node.js project
  ployzctl build local --method railpack --image myapp:latest ./app

  # Build for a specific platform
  ployzctl build local \
    --method dockerfile \
    --image ghcr.io/myorg/app:v1.2.3 \
    --platform linux/amd64 \
    .
  ```

### operation list — list build operations

  List all build operations tracked by the daemon, including completed, in-progress, and failed builds.

  ```bash
  ployzctl build operation list
  ```

  Use `--json` for full structured records.

  ```bash
  ployzctl --json build operation list
  ```

### operation get — get a build operation

  Retrieve the details of a single build operation by its ID.

  ```bash
  ployzctl build operation get <id>
  ```

  - `id` (`string`) required:

    The operation ID returned when the build was started, or listed by `operation list`.
  

  The response includes the operation record with status, method, image name, and result artifact.

## Build and deploy workflow

A typical local build-and-deploy cycle looks like this:

### Build the image

  Build from the project root. Note the digest in the output or retrieve it with `--json`.

  ```bash
  ployzctl --json build local \
    --method dockerfile \
    --image myapp:latest \
    . | tee build-result.json

  DIGEST=$(jq -r '.artifact.digest' build-result.json)
  ```

### Distribute to cluster nodes

  Push the built image to the machines that will run the workload.

  ```bash
  ployzctl image distribute \
    --digest "$DIGEST" \
    --from "$(hostname)" \
    --to machine-a \
    --to machine-b
  ```

### Deploy

  Deploy using the image digest to ensure the exact image you built is used.

  ```bash
  ployzctl deploy service web \
    --image "myapp@$DIGEST" \
    --namespace production \
    -p 80:8080
  ```

## Examples

```bash
# Build with Dockerfile from the current directory
ployzctl build local --method dockerfile --image myapp:latest .

# Build with Railpack from a subdirectory
ployzctl build local --method railpack --image myapp:latest ./src

# Build for linux/amd64 explicitly
ployzctl build local \
--method dockerfile \
--image ghcr.io/myorg/app:v1 \
--platform linux/amd64 \
.

# List all build operations
ployzctl build operation list

# Get details of a specific build
ployzctl --json build operation get op-abc123
```
