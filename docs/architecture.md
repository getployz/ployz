# Architecture

## Runtime Modes

| Mode | Target | Daemon | WireGuard | Corrosion | Gateway | DNS | eBPF | Workloads |
|------|--------|--------|-----------|-----------|---------|-----|------|-----------|
| Memory | Testing | in-process | in-memory | in-memory | embedded (tokio) | embedded (tokio) | none | none |
| Docker | macOS | host process | container (`ployz-networking`) | container (`ployz-corrosion`) | container (`ployz-gateway`) | container (`ployz-dns`) | exec in WG container | Docker containers |
| HostExec | Linux dev | host process | kernel WG / userspace | host child process | host child process | host child process | native aya | Docker containers |
| HostService | Linux prod | systemd service | kernel WG | systemd unit | systemd unit | systemd unit | native aya | Docker containers |

## Docker Mode (macOS)

```
macOS Host                           Docker Desktop VM (Linux)
┌─────────────────┐                  ┌───────────────────────────────────┐
│ ployzd daemon   │                  │ ployz-networking container        │
│                 │    WG bridge     │   wg0 interface (overlay network) │
│  OverlayBridge ─┼──UDP─over─TCP──►│   fd00::x overlay IPs             │
│  (userspace WG) │    127.0.0.1    │                                   │
│                 │                  │ ployz-corrosion (container:plz-nw)│
│  Transport::    │    bridge fwd   │   Corrosion API on overlay IP     │
│  Bridge ────────┼──127.0.0.1:8080─┼──►[fd00::x]:8080                 │
│                 │                  │                                   │
│                 │                  │ ployz-gateway (container:plz-nw)  │
│                 │                  │   HTTP proxy on overlay IP        │
│                 │                  │                                   │
│                 │                  │ ployz-dns (container:plz-nw)      │
│                 │                  │   DNS server on [overlay]:53      │
│                 │                  │                                   │
│                 │                  │ workload containers               │
│                 │                  │   Docker bridge network           │
└─────────────────┘                  └───────────────────────────────────┘
```

The daemon runs on the macOS host. WireGuard, Corrosion, Gateway, DNS, and workloads
all run inside Docker Desktop's Linux VM. Any container can route through the overlay
via the Docker bridge, but Corrosion, Gateway, and DNS need to **bind** on the node's
overlay IPv6 address (so other mesh nodes can reach them directly). They share
`ployz-networking`'s network namespace via `network_mode: container:ployz-networking`
to get access to the `wg0` interface and its overlay IPs.

## Components

### OverlayBridge

Userspace WireGuard (boringtun) + smoltcp TCP stack. Bridges the macOS host to the
container overlay network over a UDP-over-TCP tunnel to 127.0.0.1.

### eBPF TC Classifiers

In Docker mode, uses `nsenter --net=/proc/1/ns/net` inside the WG container to attach
TC hooks in the VM's host network namespace.

### DNS

Listens on `[overlay_ip]:53`. Resolves service names to instance IPs by reading
routing state from the Corrosion store.

### Gateway

Pingora-based HTTP/TCP reverse proxy. Routes by Host header to service instances
discovered from the Corrosion store.

## Container Lifecycle (Docker Mode)

All Docker-managed containers (Corrosion, Gateway, DNS) follow the same lifecycle:

1. Best-effort image pull — falls back to cached image on failure
2. Force-remove any existing container (always recreate when sharing a network namespace,
   since the parent `ployz-networking` container may have been recreated)
3. Create container with bind-mounted data directory and `container:ployz-networking` network mode
4. Start container
5. On shutdown: stop with 10s grace period, then remove

## Future: macOS Host Access

A future `ployz connect` command for macOS will:

- Spawn a local userspace WireGuard tunnel on macOS
- Spawn a local DNS resolver on macOS
- Give the macOS host direct overlay network access (can reach services by name)
- Not needed for production — only for developer access to the mesh
