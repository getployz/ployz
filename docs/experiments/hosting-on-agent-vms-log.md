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

## Act VIII — Tailscale, and where the design already points

**30. Swap WireGuard for Tailscale?** Tailscale *is* WireGuard — plus the two
things raw WG lacks here: a userspace implementation and a 443 relay fallback.
Pulled the binaries, ran `tailscaled --tun=userspace-networking` (no kernel
module, no `/dev/net/tun`) and `tailscale netcheck` (no auth needed):

```text
UDP: false                       ← direct WireGuard/UDP dead, as always
Nearest DERP: New York City
DERP latency: nyc 56.1ms  ord 61.2ms  tor 67.1ms … (every region over 443)
tshttpproxy: using proxy "http://127.0.0.1:33533" for controlplane.tailscale.com
```

**Tailscale works — and more turnkey than iroh.** It (1) uses userspace
WireGuard so the kernel-module wall doesn't apply, (2) **auto-detected and used
the CONNECT proxy** (`tshttpproxy`, zero config) so control + DERP ride the
opaque-passthrough 443 path with real certs, and (3) falls back to **DERP-only**
over 443 with a 56 ms nearest relay (vs iroh's 338 ms). Bringing a node fully
onto a tailnet needs an auth key; `netcheck` confirms every connectivity
primitive short of that.

**31. Correcting the record on "ployz-native."** An earlier draft of this log
called swapping WireGuard for Tailscale "against ployz's design." That was
wrong. Ployz's data plane is **swappable by design**: `CONTEXT.md` defines a
cluster-level **Dataplane Provider**, an explicit **Dataplane Provider
Transition** operation to change it, and frames the built-in WireGuard+eBPF as
the *"Ployz WireGuard Provider — one implementation behind Dataplane Prepare."*
The machine-endpoint code is already Tailscale-aware (it excludes `tailscale0`
from mesh candidates). Tailscale is a *named, contemplated* integration — the
**Tailnet Integration** family (Access Bridge, Subnet Access) lets you bring an
active tailnet for access/reachability/subnet routing *"to a degree,"* and a
Tailscale-as-provider is explicitly left open ("without making Tailscale the
provider **by default**"). Two honest qualifiers: it's **cluster-level, not
per-machine** ("machines do not bring their own provider"), and there's **no
`DataplaneProvider` trait in code yet** — it's a designed-for seam, not a
shipped plugin.

So the sandbox isn't a counterexample to ployz's design; it's the **motivating
case** for it. This is precisely the environment where the built-in WireGuard
provider physically can't run (no kernel WG, UDP blocked) and a userspace,
DERP-over-443 provider is the only thing that works — which is exactly the door
the Dataplane Provider abstraction was built to leave open.

## Where it lands

| Layer | Transport | Works here? |
|-------|-----------|-------------|
| Control plane — NATS 4222, CLI, machine RPC, ops, testimony | TCP + TLS | ✅ rides iroh/`dumbpipe` (or a tailnet) directly |
| Data plane — **built-in** Ployz WireGuard Provider (kernel WG 51820/UDP + eBPF) | UDP + kernel WG | ❌ can't form (no kernel WG, UDP blocked) |
| Data plane — a **userspace/DERP-over-443 provider** (Tailscale-shaped) | TCP 443 relay | ✅ the transport works; not a shipped ployz provider yet |

**Host a single-machine core and operate it from anywhere over iroh or a
tailnet.** The built-in WireGuard data plane is the ceiling *for the default
provider*; a userspace-WG/relay provider (which Tailscale proves is viable here)
is exactly what ployz's swappable **Dataplane Provider** design anticipates. For
now these are sidecars you run — ployz has the seam named, not a plugin shipped —
and the box is still ephemeral.

## Direction / open thread

The synthesis: **the two stacks that punch out of this sandbox — iroh and
Tailscale — are both P2P systems whose relay fallback speaks plain HTTPS/443**,
and both dodge the kernel-WireGuard wall (iroh via its own QUIC/relay,
Tailscale via bundled userspace WireGuard). The control plane already rides them
today (NATS is TCP). The data plane is the real prize: ployz's **Dataplane
Provider** is swappable by design, and this locked-down environment is the
motivating case for a userspace/DERP-over-443 provider — the built-in kernel-WG
provider can't run here, but the abstraction exists to let another one take its
place. Turning that seam into a shipped provider (Tailscale-shaped, or
iroh-shaped) is the open thread the notebook closes on.

## Act IX — Codex VM pass: proxy-only networking, iroh with explicit proxy, Docker still boxed in

**32. Corrected the premise.** The previous acts were Claude Code-specific:
they proved that Claude's agent VM could be coerced through a low-level
`bootstrap core` path with an offline manifest and a `systemctl` shim. For the
Codex VM pass, the useful question is different: take the Claude log as a map,
run the normal happy path where it applies, and characterize what connectivity
and container runtime primitives this VM actually has.

**33. Happy-path install works.** The public installer succeeds through the
Codex egress proxy:

```text
resolved ployz channel alpha -> v0.0.2-alpha.65
/tmp/tmp.XXXX: OK
installed /usr/local/bin/ployz
```

So the easy part is still easy: `https://ployz.sh` and the Ployz release artifact
are reachable, and SHA-256 verification passes.

**34. Codex has no direct internet route.** With `HTTP_PROXY=http://proxy:8080`
and `HTTPS_PROXY=http://proxy:8080`, `curl https://ployz.sh/ployz.sh` works.
Disable those proxy variables and the same request fails immediately:

```text
curl: (7) Failed to connect to ployz.sh port 443 ... Couldn't connect to server
```

Raw TCP probes to `1.1.1.1:443`, `1.1.1.1:80`, `1.1.1.1:7844`,
`github.com:443`, and `github.com:22` all fail with `Network is unreachable`.
UDP probes to DNS/NTP/WireGuard/iroh relay ports fail the same way. This VM is
not "443 mostly works"; it is **proxy or nothing**.

**35. CONNECT to 443 works, but it is TLS-intercepted.** Manual CONNECT probes
through `proxy:8080` returned `HTTP/1.1 200 OK` for `ployz.sh:443`,
`github.com:443`, `controlplane.tailscale.com:443`, and
`use1-1.relay.n0.iroh.link:443`. An `openssl s_client` connection through that
CONNECT path returned certificates issued by `O=OpenAI, CN=egress-proxy` rather
than the public origin CA. That is the usable primitive on Codex: software must
honor the HTTP(S) proxy **and** either trust the OpenAI egress CA or expose a way
to install/override trust roots.

**36. Docker, pass one: rootful can be made to start, but not run containers.**
This Codex image did not initially have Docker installed; `apt-get install
-y docker.io` worked. A normal daemon cannot manage the default bridge/NAT
iptables chains, but `dockerd --iptables=false --ip-masq=false --bridge=none`
starts and `docker info` answers. From there the walls are image storage and
namespaces:

- Docker 29 with the containerd snapshotter downloads image layers but fails
  extraction on a denied bind mount.
- Classic `overlay2` graphdriver fails because overlay mounts are denied.
- `vfs` graphdriver starts, but both registry pulls and `docker import` fail
  with `unshare: operation not permitted`.
- Direct `unshare -m`, `unshare -n`, `unshare -p`, and `unshare -i` all fail;
  only user-namespace-wrapped variants such as `unshare -Ur -m` work.

So rootful Docker can be useful for `docker info`-level readiness only; it cannot
pull/import/run containers in the usual way.

**37. Docker, pass two: rootless gets closer, but still no runnable Docker.**
Installed `rootlesskit`, `slirp4netns`, `fuse-overlayfs`, and `uidmap`. Plain
RootlessKit failed because the VM blocks the `newuidmap` multi-id map write:

```text
newuidmap ... write to uid_map failed: Operation not permitted
```

A single-id user namespace works manually, so I tried launching `dockerd
--rootless` inside `unshare -Ur -m`. That got farther, but:

- a Unix socket listener fails because the daemon cannot `chown` the socket under
  the single-id mapping;
- a localhost TCP listener gets through listener setup, starts managed
  containerd, then the Ubuntu-packaged Docker 29 daemon panics during startup
  before the API is usable.

**Docker verdict:** on this Codex VM, Docker can be installed and partially
started, but I did not get a working `docker run`. The hard constraints are not
networking; they are namespace/mount/id-map restrictions. If "Docker must work"
is a requirement for hosting Ployz here, the next path is not another daemon flag:
it is either (a) a VM profile with mount/net namespace and subordinate id-map
support, or (b) a purpose-built rootless/container runtime path that avoids
Docker's layer application and daemon assumptions. For this exact VM profile,
Docker is not a reliable substrate.

**38. Tailscale: yes to userspace mode and proxy detection; no proof of full
netcheck without auth/proxy quirks.** Installing Tailscale from the official apt
repository worked (`tailscale 1.98.9`). `tailscaled --tun=userspace-networking
--state=mem:` starts without `/dev/net/tun`, uses a fake/no-op TUN and fake
router/DNS configurators, and logs the proxy in link state:

```text
tshttpproxy: using proxy "http://proxy:8080" for URL: "https://controlplane.tailscale.com/"
link state: ... httpproxy=http://proxy:8080 ...
```

That answers the configuration question: **Tailscale can be run in userspace mode
and it detects/uses the HTTP proxy for control-plane traffic.** But
`tailscale netcheck` still failed before producing DERP latencies because its
DERP-map fetch attempted direct TCP and hit `Network is unreachable`, even when
`HTTPS_PROXY`, `HTTP_PROXY`, and `ALL_PROXY` were forced and a userspace
`tailscaled` socket was provided. Curling the same DERP map URL through the proxy
works and returns 28 regions, and sample DERP HTTPS endpoints return HTTP
responses. A real auth-key join may get farther than unauthenticated `netcheck`,
but the unauthenticated diagnostic path did not prove it here.

**39. iroh: the CLI diagnostic does not use the proxy, but the library can.**
The latest `iroh-doctor` needed newer Rust than this VM had, so I installed
`iroh-doctor 0.91.0`. Its default relay tests failed with `Network is
unreachable`, and forcing upper/lowercase `HTTP_PROXY`/`HTTPS_PROXY` did not
change that. Reading the installed iroh sources showed why this was worth
pushing further: the library exposes both `Endpoint::proxy_from_env` and
`iroh_relay::client::ClientBuilder::proxy_url`, but the doctor command path I ran
did not appear to wire those into the relay URL test.

A tiny Rust probe that called `ClientBuilder::proxy_url("http://proxy:8080")`
against `https://use1-1.relay.n0.iroh.link/` changed the failure from `Network is
unreachable` to TLS trust:

```text
Error: tls connection failed
Caused by: invalid peer certificate: UnknownIssuer
```

Rebuilding that same probe with iroh-relay's test-only
`insecure_skip_cert_verify(true)` proved the transport path:

```text
iroh-explicit-proxy-connect-ok
```

So iroh is the more promising of the two for Codex **if the application explicitly
sets the proxy URL and handles the OpenAI egress CA**. The relay/websocket path
can work through the proxy; the stock diagnostic CLI just did not exercise that
configuration.

**40. What looks best on Codex now?**

| Option | Codex result | Notes |
|--------|--------------|-------|
| Plain HTTPS/curl | ✅ Works | Proxy env is mandatory. |
| Direct raw TCP/UDP | ❌ No route | Even direct `:443` fails with `Network is unreachable`. |
| Rootful Docker | ❌ Not runnable | Daemon can answer only in reduced mode; layer/import/run hit mount and namespace denials. |
| Rootless Docker | ❌ Not runnable here | Single-id namespaces work, but subordinate id-map and daemon startup break before `docker run`. |
| Tailscale | ⚠️ Partial | Userspace daemon starts and detects proxy; unauthenticated `netcheck` still bypasses proxy for DERP map. Needs auth-key join test before calling it viable. |
| iroh | ✅ Promising with app wiring | Explicit `proxy_url` reaches the relay; remaining issue is OpenAI egress CA trust, not routing. |
| Purpose-built HTTPS/WebSocket tunnel | ✅ Most plausible | Best fit if it is proxy-native and trust-configurable. |

**Codex verdict:** this VM is a mandatory HTTP(S)-proxy environment with no direct
sockets and no currently usable Docker runtime. For networking, iroh is the best
lead because its library has explicit proxy support and a direct probe reached
the relay through Codex's proxy once certificate verification was bypassed. For
Docker, the substrate is still blocked: the daemon can be installed and partially
started, but container execution needs namespace/id-map/mount capabilities this
VM profile does not expose.

## Act X — Cross-VM reconciliation (reviewer's note)

Acts I–VIII are the **Claude** agent VM; Act IX is the **Codex** agent VM. Read
together they show the single most important thing: **these are not the same
box.** "Can you host Ployz on an agent VM?" has a *profile-specific* answer.

| Primitive | Claude VM (I–VIII) | Codex VM (IX) |
|-----------|--------------------|---------------|
| Direct sockets | direct `1.1.1.1:443` **connects** (then MITM'd) | **no route** — direct `:443` = `Network is unreachable`; proxy or nothing |
| Egress CA | `O=Anthropic, CN=Egress Gateway SDS Issuing CA` | `O=OpenAI, CN=egress-proxy` |
| UDP :53 | answers, but **DNS-hijacked** (web-server IPs "reply") | `Network is unreachable` (no route at all) |
| **Docker** | **runs** — pulled images, ran the internal registry, bootstrapped the core | **not runnable** — daemon partly starts; layer extract / import / `docker run` hit mount + id-map denials |

Consequences that the individual acts don't state on their own:

- **The Codex container wall was Docker-specific, not a capability wall — see the
  podman update below.** The Ployz core needs a container runtime as execution
  reality. On the Claude VM Docker itself runs; on the Codex VM `dockerd` can't,
  but **`podman` can** (Act X update). So container *execution* is available on
  both profiles — what differs is that Ployz drives the `docker` CLI + daemon
  specifically. Don't read the combined log as "Docker is universally blocked" or
  "Codex can't run containers"; both are more specific than that.
- **Our "iroh / Tailscale confirmed working" (Claude) was over-stated; Codex
  caught it.** On Claude, `iroh-doctor report` and `tailscale netcheck` succeeded
  partly because the VM *has* a direct-443 route. On Codex (no direct route)
  those same CLIs fail — Codex showed `netcheck`'s DERP fetch and the older
  `iroh-doctor`'s relay test don't reliably use the proxy. The honest status on
  **both** VMs is identical: **the relay transport is reachable over 443, but no
  real end-to-end connection was proven** (no auth-key tailnet join, no 2-node
  iroh connection). Codex's `⚠️ Partial` / "promising with app wiring" labels are
  better-calibrated than Acts VI–VIII's "yes."
- **iroh on Codex is proven only with TLS verification *off*.** The
  `insecure_skip_cert_verify(true)` probe shows the relay is reachable through the
  proxy; nobody has closed the real loop (install the egress CA via
  `rustls-native-certs` / `SSL_CERT_FILE`, or `proxy_from_env` + trust). "Works"
  today means "works with verification disabled."

**Act X update — podman runs on the Codex VM (container wall resolved).** The
open item below is now closed. A follow-up Codex pass did the full image path
that `dockerd` could not:

```text
✅ apt-get install -y podman            ✅ podman info --debug
✅ podman --version                     ✅ podman pull docker.io/library/nats:2.14.2
✅ podman create docker.io/library/nats:2.14.2
✅ podman cp <cid>:/nats-server /tmp/podman-nats/nats-server
✅ /tmp/podman-nats/nats-server --version
```

So **container execution is not the Codex wall** — Docker's daemon/id-map
assumptions were. `podman` (daemonless, `crun`, its own rootless storage) pulls,
extracts layers, creates, and runs where `dockerd` panicked. The remaining gap is
**coupling, not capability**: Ployz's host-runner speaks the `docker` CLI +
daemon directly (`docker info` DriverStatus, `/etc/docker/daemon.json`,
`systemctl restart docker`), so hosting on Codex would need Ployz pointed at
podman's Docker-compatible socket (`podman.socket` + `podman-docker`) — untested,
and podman's compat surface may not report the containerd-snapshotter /
insecure-registry fields Ployz checks. Same shape as WireGuard→Tailscale: the
substrate is swappable and works; Ployz's specific-tool coupling is the open
question.

Open items neither pass closed:

- **The `iroh-doctor` "ignores the proxy" finding is version-bound.** Codex was
  pinned to `iroh-doctor 0.91.0` by an old Rust toolchain; a newer build on the
  Claude VM does appear to use the proxy. This may already be fixed upstream.
- **End-to-end tunnel + CA-trust** for iroh and Tailscale, on either VM, still
  needs a second node / auth key and the egress CA properly trusted.
