# Hosting Ployz on an agent VM — the lab notebook

*A chronological log of what we tried, in the order we tried it, while seeing how
far a Ployz cluster gets inside the ephemeral VM behind an agentic coding session
(Claude Code on the web / Codex cloud). This is the raw journey — dead ends
included. The distilled conclusions live in
[`hosting-on-agent-vms.md`](./hosting-on-agent-vms.md); this file is the story.*

The arc, in one breath: **install is trivial → bootstrap hits three walls we
work around → a single-machine core boots and answers → then the interesting
part: can anything reach it from outside, and can it ever be more than one
machine? We try Cloudflare, ngrok, WireGuard, and iroh in turn.**

---

## Act I — What is this box?

**1. Looked at the hardware and the init system.** Ubuntu 24.04.4, kernel
`6.18.5`, 4 vCPU / 15 GiB / ~30 GB writable, `root` + passwordless `sudo`, and
essentially full capabilities (`cap_sys_admin`, `cap_net_admin`, `cap_bpf`,
`cap_sys_module`, …). Two things stood out immediately: Docker is installed but
**the daemon isn't running**, and **PID 1 is a custom process broker, not
systemd**.

**2. Started `dockerd` by hand.** Works fine — overlayfs snapshotter, cgroup v1.
So Docker is usable; it just isn't supervised for us.

**3. Checked egress.** Everything routes through an HTTP proxy
(`HTTPS_PROXY=127.0.0.1:33115`). `ployz.dev` reachable; the GitHub token is
scoped to the session's repo. First hint that the network is going to be the
whole story.

**4. Checked kernel privileges.** Full capability set — a privileged container.
`/dev/net/tun` present. But `modprobe` is missing and there's no loaded
WireGuard module. Noted for later.

## Act II — Installing Ployz

**5. Read the install path.** `scripts/ployz.sh` resolves the `alpha` channel
from `https://ployz.sh/channels/alpha.env` → a pinned, SHA-256-verified GitHub
release → installs `/usr/local/bin/ployz` → `sudo ployz host bootstrap`.

**6. Ran the public installer.** `curl -fsSL https://ployz.sh/ployz.sh | sh`:

```text
resolved ployz channel alpha -> v0.0.2-alpha.65
/tmp/tmp.XXXX: OK          # sha-256 verified
installed /usr/local/bin/ployz
```

**Install: trivial. No workarounds.** This part just works.

## Act III — Bootstrap, and three walls

**7. First `ployz host bootstrap core` → Wall #1 (TLS).**

```text
failed to download release manifest …ployz-release-linux-amd64.env:
  invalid peer certificate: UnknownIssuer
```

The binary's `ureq`/rustls client pins bundled roots and won't trust the proxy
CA.

**8. Tried the obvious fixes — installing the proxy CA into the system store,
setting `SSL_CERT_FILE`.** Still `UnknownIssuer`. `ureq` (built with the
`rustls` feature) reads neither. No env-var escape hatch exists.
→ *Later filed as issue #545.*

**9. Read the code for an offline path.** `InstallArtifactSource::try_new`
accepts an **absolute local path**, and the executor copies `LocalPath` sources
directly (no TLS). The manifest reader also honors `file://`. So we can
pre-stage everything and hand bootstrap a local manifest.

**10. Staged the substrate with `curl` (which trusts the proxy CA).** `ployzd`,
`ployz-ebpf-ctl`, `ployz-ebpf-tc` downloaded and SHA-256-matched. Then
**Wall #2 (token scope):** `nats-server` lives in a *cross-repo* GitHub release
→ **403 Forbidden** under the session-scoped token.

**11. Got `nats-server` from Docker Hub instead.** Pulled `nats:2.14.2`,
extracted `/usr/local/bin/nats-server`, repackaged it as a `.tar.gz`, re-hashed.
Docker Hub *is* reachable.

**12. Wrote a local `release.env`** with all artifact URLs as absolute local
paths, pointed bootstrap at it with `PLOYZ_RELEASE_MANIFEST_URL=file://…`.

**13. Bootstrap #2 → needs identity env.** `PLOYZ_MACHINE_ID is required`.
Supplied `PLOYZ_MACHINE_ID`, `PLOYZ_MACHINE_JOIN_NATS_URL`,
`…CLUSTER_NAME`, `…PUBLIC_IP`, `PLOYZ_GATEWAY=skip`.

**14. Bootstrap #3 → Wall #3 (no init).**

```text
failed preflight-host-ports: systemctl is-active firewalld …
  System has not been booted with systemd as init system (PID 1).
```

The host-runner picks its supervisor backend from `/etc/os-release`
(Ubuntu → systemd) and shells out to `systemctl` — which can't work here.

**15. `PLOYZ_HOST_PORTS_ASSURED_EXTERNALLY=1`** skipped the firewall probe →
next `systemctl` call: `prepare-container-runtime docker → systemctl restart
docker`. Same wall, new spot.

**16. Wrote a `systemctl` shim.** ~150 lines of Python that launches the unit
files the host-runner writes (`nats-server.service`, `ployzd-*.service`) as
plain processes, and manages `dockerd` directly. The command set the host-runner
emits is tiny — `daemon-reload`, `enable`, `restart`, `is-active`, `stop`.

**17. Pre-seeded `/etc/docker/daemon.json`** (containerd snapshotter + the
`10.198.0.0/16` internal-registry supernet as an insecure registry) and
restarted `dockerd` so the Docker-prep step became a no-op.

**18. Bootstrap #4 → exit 0.** 🎉

```text
succeeded prepare-container-runtime docker
succeeded install-artifact ployzd / nats-server / ebpf-*
succeeded start-unit nats-server.service
succeeded start-unit ployzd-control.service
succeeded start-unit ployzd-machine-core1.service
succeeded start-unit ployzd-dns.service
ployz-first-machine-bootstrap-result begin
{ "ca_pem": "…", "join_seed": "…", "machine_id": "core1",
  "nats_url": "tls://127.0.0.1:4222", "operator_seed": "…" }
```

**19. The CLI talks to the core over TLS NATS.** With the emitted CA + operator
seed: `ployz ops list` returns clean; `ployz ls` replies `ingress intent is
unconfigured` — i.e. the control service *answered*. The control plane is live.

**20. Filed issue #545** — the proxy-CA escape hatch (honor
`SSL_CERT_FILE`/`--ca-file`, opt-in so the pinned default stays).

## Act IV — Can anything reach it from outside?

The box has no inbound and no public IP, so the only option is an outbound
reverse tunnel. This is where the network policy became the whole story.

**21. Cloudflare Tunnel.** Extracted `cloudflared` from Docker Hub, ran a quick
tunnel. It *registered* a `trycloudflare.com` URL (control call over 443) but the
data path failed:

```text
Failed to dial a quic connection: timeout
UDP/TCP region1.v2.argotunnel.com:7844  FAIL
ERROR: Allow outbound QUIC traffic on port 7844 or use HTTP2.
```

**Cloudflare needs port 7844 (UDP or TCP). Both firewalled. Dead.**

**22. Checked the network-access settings.** Docs list `None / Trusted / Full /
Custom`. We were already on **Full**. So this wasn't a domain-allowlist problem.

**23. Characterized what "Full" actually is.** Not raw sockets — an inspecting
gateway:

- Direct raw TLS to `1.1.1.1:443` returns `CN=cloudflare.com` **issued by
  `O=Anthropic, CN=Egress Gateway SDS Issuing CA`** → the direct path is MITM'd.
- The **same host through the `HTTPS_PROXY` CONNECT proxy** presents cloudflare's
  *real* `O=Google Trust Services` cert, `verify ok` → the proxy path is
  **opaque passthrough**.
- Non-443 TCP times out (only 80/443 leave); UDP is dropped.

**24. ngrok.** Pulled from Docker Hub.
- First token was the wrong type (`ERR_NGROK_105`).
- Valid token, but with the proxy set → `ERR_NGROK_9009`: proxied agent
  operation is a **paid** feature.
- Proxy stripped (direct) → `x509: unknown authority` — the MITM cert again;
  ngrok pins its roots and ignores `SSL_CERT_FILE`.
- Confirmed the proxied CONNECT path reaches the **real** ngrok edge (valid
  cert). So **paid ngrok should work** — the free-plan gate is the only blocker,
  not the network.

**Verdict so far:** the only thing that leaves this box is HTTP(S) through the
CONNECT proxy on 443. Cloudflare can't use it; ngrok can, but only paid.

## Act V — The data plane: WireGuard

**25. Can a second machine ever join?** `ployz machine add` forms a **WireGuard**
overlay (UDP 51820). Two independent walls:

- `ip link add type wireguard` → `Unknown device type`. **No kernel WireGuard**
  (no `/lib/modules` to load, not built in). `/dev/net/tun` *does* exist, so
  *userspace* WG (`wireguard-go`/`boringtun`) is theoretically possible — but
  ployz drives kernel WG.
- **UDP is blocked.** A definitive test: a raw DNS query on `:53` to
  *web-server IPs that run no DNS* still got answers → all `:53/udp` is
  transparently captured by a managed resolver, and every other UDP port is
  dropped. WireGuard is UDP-only, so no custom port helps.

**Multi-machine is structurally impossible across these VMs.** Single-machine
never needed the overlay, which is why the core still booted.

## Act VI — iroh: the one that punches out

**26. Probed iroh's relays.** iroh is QUIC/holepunch (UDP, dead here) but is
*designed* to fall back to a relay over HTTPS. Through the proxy:

```text
iroh relays on :443   -> real Let's Encrypt cert, verify ok
GET /ping             -> 200
GET /relay (ws upgrade) -> 101 Switching Protocols
```

The relay's **websocket transport** — iroh's restricted-network fallback —
accepts the connection over TCP/443.

**27. Built `iroh-doctor` and ran a real connectivity report.**

```text
udp_v4: false, udp_v6: false          # UDP dead, as expected
relay_latency.https:
  use1-1.relay.n0.iroh.link : 338ms   # all four relays reachable over 443
  euc1-1 : 653ms, usw1 : 855ms, aps1 : 1.43s
preferred_relay: Some("use1-1.relay.n0.iroh.link")   # node HOMED on a relay
```

**iroh works.** Zero UDP, but it fell back to the HTTPS/websocket relay,
reached every relay, and picked a home relay — over the proxy. A relay-homed
node is a reachable iroh endpoint. **The one connectivity option that sails
through where WireGuard, Cloudflare, and free ngrok all wall off.**

## Act VII — Composing it

**28. WireGuard *over* iroh?** Since iroh gives a working carrier and WG gives a
routable L3 interface, layer them. Confirmed the substrate: `ip tuntap add mode
tun` creates a working interface here, so userspace WG (`wireguard-go`) would
run. Feasible — with real sharp edges (QUIC-datagram MTU squeeze, datagram-vs-
stream choice, double encryption, ~338 ms relay latency). A "because you can"
build; for reaching one service, plain iroh TCP-forward is simpler.

**29. NATS over iroh — the clean one.** The ployz control plane is plain TCP+TLS
(`nats-server:4222`), so it rides iroh's TCP-forward (`dumbpipe`) directly — no
WireGuard, no TUN, no MTU tuning. `dumbpipe` forwards bytes, so NATS TLS + nkey
auth stay **end-to-end** between a remote CLI and the sandbox's nats-server.

```text
# sandbox:  dumbpipe listen-tcp --host 127.0.0.1:4222      -> iroh ticket
# laptop:   dumbpipe connect-tcp --addr 127.0.0.1:4222 <ticket>
# laptop:   PLOYZ_NATS_URL=tls://127.0.0.1:4222 … ployz ops list
```

This is the payoff: **a Ployz core hosted in a Claude sandbox, operated from
anywhere over iroh** — because the entire control plane is NATS/TCP.

## Where it lands

| Layer | Transport | Over iroh? |
|-------|-----------|------------|
| Control plane — NATS 4222, CLI, machine RPC, ops, testimony | TCP + TLS | ✅ rides `dumbpipe` directly |
| Data plane — machine overlay (WireGuard 51820/UDP, kernel), gateway eBPF | UDP + kernel WG | ❌ can't form |

**Host a single-machine core, operate it from anywhere over iroh; the data plane
is the ceiling.** Everything above is a sidecar you run — ployz doesn't
orchestrate iroh — and the box is still ephemeral.

## Direction / open thread

The goal we're steering toward: **run the entire thing over iroh** as the
universal transport — control plane confirmed, data plane the open question
(userspace-WG-over-iroh is the candidate, with the caveats above; making *ployz*
use it would need a userspace-WG fallback and a pluggable/TCP-wrappable dataplane
transport it doesn't have today). One more investigation is pending before we
close the notebook.
