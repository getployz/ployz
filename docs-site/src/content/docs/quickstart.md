---
title: "Get started with Ployz: deploy your first workload"
description: "Install Ployz, initialize a mesh, add a machine, deploy a workload, and verify it is running — from zero to a live cluster in under five minutes."
llms:
  summary: "Install Ployz, initialize a mesh, add a machine, deploy a workload, and verify it is running — from zero to a live cluster in under five minutes."
---
This guide takes you from a fresh machine to a running Ployz cluster with a deployed workload. You will install the CLI and daemon, initialize a mesh network, add a node, deploy an application, and confirm it is serving traffic. The entire sequence runs in under five minutes on a supported machine.

:::note
Ployz runs on Linux (x86\_64 and aarch64) and macOS (x86\_64 and Apple Silicon). On macOS, Docker Desktop or OrbStack must be running before you start — Ployz uses Docker as its container runtime on macOS.
:::

### Install Ployz

  Run the one-line installer. It detects your platform, downloads the latest release, installs the binaries to `~/.local/bin`, and registers the daemon as a user service.

  ```bash
  curl -fsSL https://ployz.sh | bash
  ```

  The installer prints a summary of what it installed and where. When it finishes, confirm the daemon is running:

  ```bash
  ployzctl status
  ```

  Expected output:

  ```text
  daemon: running
  runtime: docker          # macOS default
  socket: /tmp/ployz/ployzd.sock
  ```

  :::note
    If `ployzctl` is not found after install, add `~/.local/bin` to your `PATH`:

    ```bash
    export PATH="$HOME/.local/bin:$PATH"
    ```

    Add this line to your shell profile (`~/.bashrc`, `~/.zshrc`, etc.) to make it permanent.
  :::

### Initialize a mesh

  A mesh is the WireGuard overlay network that connects your cluster nodes. Initialize one on this machine and give it a name that identifies the cluster.

  ```bash
  ployzctl mesh init my-cluster
  ```

  This command generates a WireGuard key pair for this node, starts the overlay interface, initializes NATS as the cluster state substrate, and prints the join token other machines will use to enter the mesh.

  ```text
  mesh initialized: my-cluster
  node id:   node_01jx...
  overlay:   10.101.0.1/24
  join token: ployz-join-...
  ```

  Save the join token. You will pass it to `machine add` when joining additional nodes.

### Add a machine

  To add a second machine to the cluster, run `machine add` from the first machine. Pass the SSH address of the new node as a positional argument. Ployz SSHs in, installs the daemon if needed, joins it to the mesh, and verifies reachability.

  ```bash
  ployzctl machine add --network my-cluster user@203.0.113.42
  ```

  Expected output:

  ```text
  machine added: node_02jx...
    address:  203.0.113.42
    overlay:  10.101.0.2/24
    status:   active
  ```

  Confirm both machines appear in the cluster:

  ```bash
  ployzctl machine ls
  ```

  ```text
  ID             REGION  ROLE        STATUS
  node_01jx...   local   active      running
  node_02jx...   -       active      running
  ```

### Deploy a workload

  Deploy a container image to the cluster using `deploy service`. Provide a service name, the image, the namespace, and the port mapping.

  ```bash
  ployzctl deploy service web \
    --image nginx:alpine \
    --namespace default \
    --publish 80:80
  ```

  Ployz moves through visible phases — plan, apply, commit — and reports each one. The deploy completes or fails cleanly. When the command returns, the workload is either live or the deploy has failed with a clear error.

  :::note
    Each deploy is atomic. The routing flip to the new instance only happens after the durable commit point. If something goes wrong before commit, the previous version keeps serving traffic and you can retry safely.
  :::

### Verify the workload

  Check that all machines and workloads are running:

  ```bash
  ployzctl status
  ```

  ```text
  daemon: running
  runtime: docker
  socket: /tmp/ployz/ployzd.sock
  ```

  To inspect what is deployed in a namespace, preview the current state against your manifest:

  ```bash
  ployzctl deploy preview -f deploy.toml
  ```

  Make a request to the workload through its cluster-internal address (requires being on the overlay network):

  ```bash
  curl http://web.default.cluster.local
  ```

### Branch for a PR environment (optional)

  To create an isolated environment for a pull request, branch the namespace. Services are copied by default; volumes start fresh.

  ```bash
  ployzctl branch apply default pr-42 \
    --service-mode branch \
    --volume-mode fresh
  ```

  Deploy the PR build into the branched namespace:

  ```bash
  ployzctl deploy service web \
    --image nginx:alpine \
    --namespace pr-42 \
    --publish 80:80
  ```

  When done, the branch namespace can be removed with another deploy targeting that namespace.

## Next steps

### [Installation details](/installation)

  Manual install, building from source, platform notes, and daemon configuration options.

### [What is Ployz?](/introduction)

  Understand the primitives-not-policies philosophy and how Ployz compares to Kubernetes.
