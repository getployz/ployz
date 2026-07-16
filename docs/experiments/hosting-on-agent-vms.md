# Hosting Ployz on a Claude Code / Codex cloud VM

*An experiment, not a support statement. Everything below was run inside the
ephemeral sandbox that backs an agentic coding session (Claude Code on the web;
Codex cloud VMs are the same shape). The goal was to answer three questions for
a "for the lulz" blog post: **how long does the box stay up, is Ployz
installable, and can server init be made to "just work"?***

TL;DR: **Yes — a single-machine Ployz core boots and serves its NATS control
plane inside the sandbox.** But it is not free. Three sandbox properties fight
the stock install path, and each needs a deliberate workaround. The reusable
recipe is [`host-on-agent-vm.sh`](./host-on-agent-vm.sh).

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
container, so eBPF, `nft`, WireGuard, and namespaces are all *permitted*. The
constraints are elsewhere.

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

## 6. Takeaways

- **Install: trivial.** The public channel + SHA-256 verification works as-is.
- **Bootstrap: possible, not turnkey.** Three sandbox realities — a pinned-root
  TLS client, a repo-scoped token, and no init system — each need a deliberate
  workaround. [`host-on-agent-vm.sh`](./host-on-agent-vm.sh) folds them into one
  idempotent script.
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
