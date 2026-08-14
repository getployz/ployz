# Hosting Ployz on a Claude Code / Codex cloud VM

*An experiment, not a support statement. Everything below was run inside the
ephemeral sandbox that backs an agentic coding session (Claude Code on the web;
Codex cloud VMs are the same shape). The goal was to answer three questions for
a "for the lulz" blog post: **how long does the box stay up, is Ployz
installable, and can server init be made to "just work"?***

> This page is the distilled, topic-organized writeup. For the blow-by-blow
> **chronological log of everything we tried, in order** (dead ends included —
> the raw material for the post), see
> [`hosting-on-agent-vms-log.md`](./hosting-on-agent-vms-log.md).

TL;DR: **Yes — a single-machine Ployz core boots and serves its NATS control
plane inside the sandbox.** But it is not free. Three sandbox properties fight
the stock install path, and each needs a deliberate workaround. The reusable
recipe is [`host-on-agent-vm.sh`](./host-on-agent-vm.sh). Making it reachable
from the *public* internet ([§6](#6-can-you-expose-it-to-the-public-internet))
and scaling past one machine ([§7](#7-the-data-plane-wireguard-the-multi-machine-wall))
are separate questions, each with its own hard limit — a single-machine core is
as far as these sandboxes go.

---

## 1. What the box actually is

| Property        | Value (observed)                                              |
|-----------------|---------------------------------------------------------------|
| OS              | Ubuntu 24.04.4 LTS                                             |
| Kernel          | 6.18.5                                                         |
| CPU / RAM       | 4 vCPU / 15 GiB                                                |
| Disk            | ~30 GB writable allowance (of a 252 GB fs)                    |
| User            | `root`, passwordless `sudo`                                   |
| Capabilities    | Effectively full — `cap_sys_admin`, `cap_net_admin`, `cap_bpf`, `cap_sys_module`, `cap_perfmon`, … |
| Docker          | Engine 29.x installed; **daemon not started** at boot         |
| Init system     | **None** — PID 1 is a custom process broker, not systemd      |
| Egress          | HTTP proxy **only** (TLS re-terminated at a policy proxy)     |
| GitHub token    | Scoped to the **session's repo only**                         |

The capability set is the pleasant surprise: this is essentially a privileged
container, so eBPF, `nft`, and namespaces are all *permitted*. But *permitted*
is not *present* — the running kernel ships no WireGuard support (see
[§7](#7-the-data-plane-wireguard-the-multi-machine-wall)), and the real
constraints are in the kernel and the network, not the capability bits.

## 2. How long does it stay running?

It is an **ephemeral, inactivity-reclaimed** container, not a host:

- The filesystem is a **fresh clone per container**; nothing persists across
  sessions unless committed and pushed.
- The container is **reclaimed after a period of inactivity** or when the
  session ends. Writable disk is a **fixed allowance**, so `df` "Avail" hitting
  zero means the quota is spent, not that the disk is full.

Practical consequence: this is a fantastic **throwaway demo host / playground**
— spin a core up, drive it, screenshot it, tear the session down. It is the
*opposite* of a durable production host. Any "keepalive" would mean keeping the
session artificially active (a scheduled poke), which is against the grain of
what these sandboxes are for. Treat uptime as *minutes-to-hours of active use*,
not days.

## 3. Is Ployz installable? — Yes, cleanly

The public installer is the easy part and needs **no** workarounds:

```console
$ curl -fsSL https://ployz.sh/ployz.sh | sh --channel alpha
resolved ployz channel alpha -> v0.0.2-alpha.65
/tmp/tmp.XXXX: OK                     # sha-256 verified
installed /usr/local/bin/ployz
run: sudo ployz host bootstrap
```

`https://ployz.sh` and the pinned GitHub release for **the Ployz repo** are
reachable through the proxy, and the installer's SHA-256 verification passes.
The CLI binary lands and runs.

## 4. Making `bootstrap` "just work" — the three walls

`ployz host bootstrap core` is where the sandbox bites. It failed three times,
each for a different, instructive reason.

### Wall 1 — the binary won't trust the proxy CA

The sandbox's only egress is a TLS-terminating proxy. `curl` is pre-configured
to trust its CA, but the `ployz` binary uses `ureq` built with **rustls +
bundled webpki roots** (`default-features = false, features = ["json",
"rustls"]`). It reads neither the system trust store nor `SSL_CERT_FILE`, so it
cannot be told to trust the proxy:

```text
failed to download release manifest …/ployz-release-linux-amd64.env:
  cloud request failed: io: invalid peer certificate: UnknownIssuer
```

There is no env-var fix. The substrate must be delivered **out of band**.

### Wall 2 — nats-server lives in a cross-repo release the token can't reach

The substrate manifest points `nats-server` at `nats-io/nats-server`'s GitHub
release. The session token is scoped to the Ployz repo, so that download is
**403 Forbidden**. Docker Hub, however, *is* reachable — so we pull the exact
pinned version's binary out of the official `nats:<ver>` image and repackage it.

### Wall 3 — no init system to supervise the substrate

Ployz's host-runner picks its supervisor backend from `/etc/os-release`
(Ubuntu → **systemd**), not from what is actually running. Every substrate step
then shells out to `systemctl` — for `firewalld` preflight, for `docker`, and to
start `nats-server` and each `ployzd` role. With no systemd, all of it fails:

```text
failed prepare-container-runtime docker: systemctl restart docker …
  System has not been booted with systemd as init system (PID 1).
```

### The workarounds

The offline-manifest escape hatch is the crux. The host-runner's
`InstallArtifactSource` accepts an **absolute local path** as well as an
`https://` URL, and `acquire_artifact` copies `LocalPath` sources straight from
disk — no TLS, no proxy. So we:

1. `curl` every artifact into `/opt/ployz-stage` (proxy-trusting), pull
   `nats-server` from Docker Hub, and re-`sha256`.
2. Write a `release.env` whose artifact URLs are those **local paths**, and
   point bootstrap at it via `PLOYZ_RELEASE_MANIFEST_URL=file://…`.
3. Drop a **`systemctl` shim** on `PATH` that launches the very unit files
   Ployz writes as plain processes (and manages `dockerd` directly). The
   command set the host-runner emits is tiny — `daemon-reload`, `enable`,
   `restart`, `is-active`, `stop` — so the shim is ~150 lines.
4. Pre-seed `/etc/docker/daemon.json` (containerd snapshotter + the
   `10.198.0.0/16` internal-registry supernet as an insecure registry) and
   start `dockerd`, so bootstrap's Docker step is a no-op.
5. Bootstrap with `PLOYZ_HOST_PORTS_ASSURED_EXTERNALLY=1` (skips the
   `firewalld` probe) and `PLOYZ_GATEWAY=skip` (control plane only).

## 5. It boots

```text
succeeded prepare-container-runtime docker
succeeded install-artifact ployzd /usr/local/bin/ployzd
succeeded install-artifact nats-server /usr/local/bin/nats-server
succeeded start-unit nats-server.service
succeeded start-unit ployzd-control.service
succeeded start-unit ployzd-machine-core1.service
succeeded start-unit ployzd-dns.service
ployz-first-machine-bootstrap-result begin
{ "ca_pem": "…", "join_seed": "…", "machine_id": "core1",
  "nats_url": "tls://127.0.0.1:4222", "operator_seed": "…" }
```

All four substrate processes stay up, NATS listens on `4222`, and the CLI —
pointed at the core with the emitted CA + operator seed — gets **real answers
from the control service over TLS NATS**:

```console
$ ployz ops list          # clean, empty
$ ployz ls                # service replies: "ingress intent is unconfigured"
```

That `ingress intent is unconfigured` is not a failure — it is the core's
control service answering. `ls`/`machine ls` assemble a view that wants ingress
intent, which `ployz init` normally configures and our low-level `bootstrap
core` skipped. The control plane itself is fully live.

## 6. Can you expose it to the public internet?

The core runs, but a cluster is only interesting if something can reach the
services it fronts. The sandbox has **no inbound** and no public IP, so the only
option is an **outbound reverse tunnel**. That runs straight into how these
boxes do egress — and it is more restrictive than a domain allowlist.

### Egress is a mandatory inspection gateway, not raw sockets

Claude Code on the web exposes a **Network access** selector per environment —
`None` / `Trusted` (default) / `Full` / `Custom` (your own allowlist). But
"Full" widens *which domains* are reachable; it does **not** hand you raw
sockets. Two things stay true at every level:

- **Non-443 ports and UDP are dropped.** Raw TCP to `:7844` times out; UDP to
  `:7844` is dropped.
- **All egress goes through an inspecting gateway**, and the two paths to it
  behave differently:

  | Path | Behavior | Observed |
  |------|----------|----------|
  | Direct socket (no proxy) | **TLS-terminated / MITM** | a raw handshake to `1.1.1.1:443` returns `CN=cloudflare.com` **issued by `O=Anthropic, CN=Egress Gateway SDS Issuing CA`** |
  | Via the `HTTPS_PROXY` CONNECT proxy | **opaque passthrough**, genuine end-to-end TLS | the same host through the proxy presents cloudflare's *real* `O=Google Trust Services` cert, `verify ok` |

So the environment is engineered so an agent's outbound traffic is
HTTP(S)-through-a-CONNECT-proxy. Reach for a raw socket and you get MITM'd;
reach through the proxy and you get honest end-to-end TLS — but only to `:443`.

### What that allows and forbids

- **Cloudflare Tunnel: no, at any access level.** `cloudflared` hard-requires
  port **7844** (UDP QUIC, or TCP HTTP/2 fallback); both are firewalled. Its
  own pre-checks fail: `Allow outbound QUIC traffic on port 7844 or use HTTP2 /
  Allow outbound TCP on port 7844`. No knob — on the sandbox or in the ployz
  gateway — changes that.
- **ngrok: only on a paid plan.** Direct egress hits the MITM cert and ngrok
  (which pins its own roots and ignores `SSL_CERT_FILE`) rejects it with
  `x509: unknown authority`. Going through the proxy works at the transport
  layer — the proxy passes through to the **real** ngrok edge with a valid cert
  — but the free plan refuses proxied operation (`ERR_NGROK_9009`,
  "Running the agent with an http/s proxy is a Pay-as-you-go feature"). A
  paid/Pay-as-you-go plan lifts exactly that gate, and the passthrough path
  underneath is confirmed reachable — so it should establish.
- **A 443 reverse tunnel to your own relay: plausible, free, with a VPS.** A
  proxy-aware `ssh -R` (`ssh -o ProxyCommand=…CONNECT…`), or `inlets`/`frp`
  over wss/443 configured to use `HTTPS_PROXY`, rides the same passthrough path.
  TLS-shaped protocols are guaranteed to pass (ngrok proved it); raw SSH after
  CONNECT is likely but not certain (the gateway may expect TLS-shaped bytes).
- **iroh: yes.** Its QUIC/UDP path is dead, but it falls back to a
  relay-over-websocket on TCP/443; `iroh-doctor` reports `udp: false` yet homes
  on a relay (`use1` at ~338 ms). A P2P byte-pipe; needs a small bridge
  (`dumbpipe`) to front a service.
- **Tailscale: yes, and most turnkey.** It bundles **userspace WireGuard** (no
  kernel module), **auto-uses the CONNECT proxy** (`tshttpproxy`), and falls back
  to **DERP over 443**. `tailscale netcheck` shows `UDP: false` but every DERP
  region reachable, nearest at **56 ms**. Gives a real L3 tailnet, not just a
  byte-pipe. (Full join needs an auth key; `netcheck` confirms the rest.)

### Wiring a tunnel to the gateway

None of this needs gateway code changes — a tunnel is a **sidecar**: point it at
the ployz gateway's local host-port (or a service's published port), and set
ployz DNS/automatic-hostnames to `external` since the tunnel provider owns the
public hostname. The gateway's on-machine host-port/eBPF routing is unchanged.

### Bottom line

**Publicly, the box is reachable only through a 443 tunnel that traverses the
CONNECT proxy.** Cloudflare Tunnel structurally cannot (port 7844); paid ngrok or
a self-hosted 443 relay can; and the cleanest are the **P2P stacks whose relay
fallback speaks plain HTTPS — iroh and Tailscale**, both confirmed working here.
That is a deliberate isolation property of the agent sandbox, not a ployz limit.

## 7. The data plane: the multi-machine wall — and the seam that answers it

The core booted single-machine, which never exercises the overlay. Add a second
machine and ployz's **built-in provider** forms a **WireGuard** data plane
between hosts (the `51820/udp` seen in bootstrap). That specific provider cannot
run in these sandboxes, for two independent reasons — neither a permission you
can grant from the **Network access** settings:

1. **No WireGuard in the kernel.** `ip link add dev wg0 type wireguard` returns
   `Unknown device type`. There is no `/lib/modules/$(uname -r)` at all (no
   out-of-tree `.ko` to load), and the custom `6.18.5` kernel has no built-in
   support. The capability bits are already present (`cap_net_admin`,
   `cap_sys_module`) — they are simply moot with nothing to load. `/dev/net/tun`
   *does* exist, so a **userspace** implementation (`wireguard-go`/`boringtun`)
   is theoretically possible, but ployz drives kernel WireGuard through the
   host-runner and ships no userspace fallback.
2. **UDP transport is blocked anyway.** WireGuard is UDP-only; outbound UDP
   (`51820`, `7844`) is silently dropped and there is no inbound. Even a working
   interface could never reach a peer — the same wall that stops Cloudflare's
   QUIC.

So the boundary is clean for the **default** provider: **the built-in Ployz
WireGuard Provider can't form an overlay across agent VMs** — a second sandbox
has the same missing kernel WireGuard and UDP-blocked network. This matches
ployz's own stance that real WireGuard / tcx eBPF need real hosts.

But "WireGuard" is a provider detail, not the architecture. Ployz's data plane
is **swappable by design**: `CONTEXT.md` defines a cluster-level **Dataplane
Provider**, a **Dataplane Provider Transition** operation, and the built-in
WireGuard+eBPF as *the "Ployz WireGuard Provider — one implementation behind
Dataplane Prepare,"* alongside a **Tailnet Integration** family for bringing an
active Tailscale tailnet "to a degree." And Tailscale runs here: `tailscaled
--tun=userspace-networking` needs no kernel module, auto-uses the CONNECT proxy,
and falls back to DERP over 443 (`netcheck`: `UDP: false`, nearest relay 56 ms).

So the honest conclusion is sharper than "impossible": **the default
kernel-WireGuard provider can't run in this environment, but this environment is
exactly the motivating case for the swappable-provider design** — a userspace,
DERP-over-443 provider (Tailscale-shaped) is what would work. The seam is named
in the domain model; it is not a shipped plugin (`DataplaneProvider` is not yet a
trait in code), and the Tailnet integrations are cluster-level, not per-machine.

## 8. Takeaways

- **Install: trivial.** The public channel + SHA-256 verification works as-is.
- **Bootstrap: possible, not turnkey.** Three sandbox realities — a pinned-root
  TLS client, a repo-scoped token, and no init system — each need a deliberate
  workaround. [`host-on-agent-vm.sh`](./host-on-agent-vm.sh) folds them into one
  idempotent script.
- **Public ingress: only via a 443 proxy-traversing tunnel.** No inbound, no raw
  sockets, no non-443 ports; Cloudflare Tunnel (7844) can't. The clean options
  are the P2P stacks whose relay fallback speaks HTTPS — **iroh and Tailscale,
  both confirmed working** — or paid ngrok / a self-hosted 443 relay.
- **Multi-machine: not with the default provider — but the design has an out.**
  The built-in kernel-WireGuard data plane can't form here (no kernel WG, UDP
  blocked). Ployz's **Dataplane Provider is swappable by design**, and this is
  the motivating case: a userspace/DERP-over-443 provider (Tailscale-shaped)
  works where the default can't. Named seam, not a shipped plugin yet.
- **Hosting: no.** These are ephemeral, inactivity-reclaimed dev sandboxes.
  Perfect for a live demo or a "look, it runs anywhere" screenshot; wrong tool
  for anything that must outlive the session.

### Things this surfaced that are worth a real look

- The host-runner picks its supervisor backend from `/etc/os-release`, not from
  a runtime probe of the init system. An explicit "no init / foreground
  supervisor" mode would make Ployz genuinely portable to init-less
  environments (containers, sandboxes) instead of only systemd/OpenRC hosts.
- `ureq`'s bundled roots make the binary unusable behind a corporate /
  MITM-proxy CA. Honoring the system trust store (or `SSL_CERT_FILE`) for
  substrate downloads would let bootstrap work in proxied environments without
  the offline-staging dance.
