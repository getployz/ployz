# The Mesh Provider Seam and the Principal

First-draft spec from the wayfinder ticket [Decide: the mesh-provider seam
and the Principal abstraction](https://github.com/getployz/ployz/issues/787).
Companion to [the Corrosion row model](corrosion-row-model.md). This governs
the coreless v2 design.

## The provider is a cluster-wide, init-fixed choice

The mesh provider is chosen once, at `ployz init`, and is uniform across
every machine in the cluster. Switching providers is teardown + fresh
install, never a migration. Mixing providers inside one cluster is refused
as a second system.

v1 ships exactly one provider: builtin WireGuard. The Tailscale column in
the matrix below is spec text that proves the seam is not secretly
WG-shaped; no Tailscale code ships until real demand arrives.

## The seam: an enum with three responsibilities

One implementation exists, so the seam is an enum, not a trait (a trait
arrives with the second real implementation, additively):

```rust
enum MeshProvider {
    BuiltinWireguard(WgConfig),
    // additive when it ships: Tailscale(TsConfig),
}

impl MeshProvider {
    /// 1. The address gossip and the HTTP API bind to.
    fn bind_ip(&self) -> IpAddr;

    /// 2. Mint this machine's transport identity at join.
    ///    builtin: WG keypair + derived IPv6 + door-allocated IPv4 /24
    ///    tailscale: read the local tailnet IP
    fn provision_join(&self, ...) -> Result<MachineTransport>;

    /// 3. Make roster addresses reachable.
    ///    builtin: converge the WG peer set from machines ∪ peers rows
    ///    tailscale: no-op — the tailnet owns peers
    fn converge_peers(&self, roster: &Roster) -> Result<()>;
}
```

Address→identity mapping is deliberately *not* a method: it is one uniform
rule (below) living once in the HTTP layer. The CLI's laptop dial — a
userspace-WG connector under builtin, plain sockets on a tailnet — is a
small CLI-side connector matched on the same enum.

## Builtin addressing: dual-stack, each family doing one job

```
┌─ CONTROL PLANE — IPv6, derived, never allocated ────────────────────┐
│                                                                     │
│  cluster prefix  = fd | sha256(cluster_id)[0..40]          → /48    │
│  machine subnet  = prefix | sha256(wg_pubkey)[0..64]       → /112   │
│  machine address = its ::1                                          │
│                                                                     │
│  · exists at keygen — before the row, before the door answers       │
│  · carries: Corrosion gossip, HTTP API, SSE, CLI dial, Principal    │
│  · zero allocation, zero races, zero healing, zero addressing rows  │
│  · containers never see it                                          │
│                                                                     │
├─ CONTAINER PLANE — IPv4, allocated once, self-healing ──────────────┤
│                                                                     │
│  machine subnet = one /24 under the init-chosen prefix              │
│                   (default 10.210.0.0/16), bridge .1, containers .2+│
│                                                                     │
│  · carries: container↔container traffic, DNS A records,             │
│    gateway upstreams — containers are pure IPv4                     │
│                                                                     │
└─ one WG tunnel per machine pair; allowed-ips = { v6 /112, v4 /24 } ─┘
```

Because allowed-ips covers the whole /24, cryptokey routing authenticates
container-sourced packets too, and cross-machine container IPv4 routes by
prefix with zero rows.

The /16 default caps a cluster near 250 machines — exactly where the map
already says cells, never a bigger cluster. The prefix is configurable at
`ployz init` for LAN-conflict escapes.

Implementation check item: verify the pinned Corrosion gossips over ULA
IPv6 before wiring `bind_ip` to it; the spike ran IPv4.

### IPv4 /24 allocation and self-heal

The admitting machine (the door handler) allocates the /24 at admission:

1. Read the roster; pick the lowest free subnet.
2. Write the machines row — the row is the claim.
3. Courtesy re-read (fixed 1–2 s, no correctness weight) before answering
   the joiner; on a lost race, re-allocate and rewrite before replying.
4. Reader law is the backstop: duplicate subnets adjudicate by canonical
   machine-name order.

**Self-heal.** A partition can still birth a surviving duplicate, and 200
unattended cloud-init joins cannot end in "operator re-joins it". Keeper's
converge already reads the full roster, so the losing machine detects "my
`subnet_v4` duplicates a lower machine name" and re-runs the allocation recipe
itself: lowest free pick, rewrite own row, courtesy re-read. This is the
**one named exception to the row-ownership law**: a machine may rewrite
*its own* machines-row `transport.subnet_v4`, and only to exit a
name-ordered duplicate loss — a deterministic trigger completing the
admission the operator already commanded. The heal renumbers that
machine's containers (restart-heal, doctor-visible); its control-plane
identity is IPv6-derived and never moves, so gossip, the API, and live
SSE streams ride through untouched. The WG pubkey stays write-once;
everything else stays operator-authority.

## Identity: one resolution rule, the Principal enum

Caller identity is **source address looked up in the roster** (machines ∪
peers). The provider's job is guaranteeing source addresses cannot be
spoofed — builtin by cryptokey routing, Tailscale by the tailnet. No
per-request certs, headers, or whois calls. Resolution produces a
Principal; handlers authorize against the variant and never touch
addresses or keys themselves:

```rust
enum Principal {
    /// cryptokey-routed source → machines row
    Machine(MachineName),
    /// cryptokey-routed source → peers row (operator laptop, Cloud)
    Peer(PeerName),
    /// join-token secret presented at the public join door;
    /// honored at exactly one endpoint: join
    ApiToken(TokenName),
    // future variants additive
}
```

`Machine` and `Peer` are distinct because their authority differs:
machines write testimony, peers issue commands. Laptop and Cloud share
`Peer` — single-operator trust makes them the same authority.

**The rejection rule.** Any transport that cannot arrive as a Principal
variant plus the matching `MachineTransport` or `PeerTransport` variant on a
roster row is rejected as a second system. There is no side door, no ambient
identity, no "trusted network" mode.

## The peers table

Non-machine mesh peers (the operator's laptop, Cloud) live in their own
`peers` table, not kind-tagged into `machines` — roster readers
(placement, folds, `machine ls`) never carry a "skip operators" filter,
and Keeper's mesh diet reads both tables. Same document discipline as
every row-model table:

```sql
-- Non-machine mesh peers: operator laptops and Cloud. Operator authority;
-- swept by peer rm. Document carries PeerTransport, whose variants have no
-- IPv4 subnet (peers run no containers).
CREATE TABLE peers (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    name TEXT GENERATED ALWAYS AS (json_extract(document, '$.name')) VIRTUAL
);
CREATE INDEX peers_name ON peers (name);
```

Peers get a derived IPv6 /112 like machines and no `subnet_v4`.

## The transport unions on roster rows

Machine and peer documents carry separate internally tagged unions, matched
exhaustively in code. A machine always owns one container subnet. A peer never
does:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MachineTransport {
    Wireguard {
        /// write-once identity
        pubkey: WgPublicKey,
        /// derived from pubkey; stored so non-Rust readers need no
        /// hash reimplementation; doctor verifies derivation
        addr_v6: Ipv6Addr,
        /// None = NAT'd/roaming (WG learns it from the handshake)
        endpoint: Option<SocketAddr>,
        /// allocated at admission; the one self-healing field
        subnet_v4: Ipv4Net,
    },
    Tailscale {
        ip: Ipv4Addr,
        subnet_v4: Ipv4Net,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PeerTransport {
    Wireguard {
        pubkey: WgPublicKey,
        addr_v6: Ipv6Addr,
        endpoint: Option<SocketAddr>,
    },
    Tailscale {
        ip: Ipv4Addr,
    },
}
```

`MachineDocument.transport` cannot omit or null its `subnet_v4`.
`PeerDocument.transport` has no such field, and a peer transport containing
`subnet_v4` is malformed rather than an additive extension. This makes a peer
structurally unable to claim container address space.

Roster acceptance requires the accepted `ClusterDocument` as context. The
roster reader parses the row, compares the transport `kind` to
`ClusterDocument.provider`, and only then admits the row to a name or subnet
claim fold. A mismatch is skipped and surfaced to `doctor`; it cannot enter the
accepted roster. An unknown `kind` fails to parse and lands
in the row model's existing skip-unparseable guard.

## Join: one public door, token in hand

Every machine serves one **public join-only HTTPS endpoint** — the
Kubernetes bootstrap-token shape, chosen because admission must survive
200 simultaneous cloud-init joiners:

- The join string embeds the token name and secret (hashed
  in the row per the row model), the **cluster door-cert fingerprint**,
  and one or more member endpoints. The joiner pins TLS by fingerprint —
  no hostnames, no CA machinery. Exact join-string UX belongs to the
  join-token UX ticket.
- TLS is one **cluster door keypair**: minted by `ployz init`, handed to
  each joiner inside the join response, machine-local at rest, never a
  row. A real CA arrives only if a future ticket needs per-machine certs.
- The call arrives as `Principal::ApiToken(token-id)` — the variant ships
  in v1 and is honored at exactly this endpoint.
- The request body declares what is joining: a **machine** (public
  endpoint optional, gets a `machines` row + /24) or a **roaming peer**
  (gets a `peers` row). One admission door for machines, laptops, Cloud.

**NAT.** A NAT'd machine joins fine: it dials out for HTTPS and outbound
WG handshakes, its row carries `endpoint: None`, WG roaming learns its
address, keepalive holds the mapping. NAT'd ↔ NAT'd machine pairs are
builtin's named ceiling — no hole punching, no relays; the refusal names
the resolver: *use the Tailscale provider*. A fully private cluster joins
over its private network (the token carries private endpoints) or by SSH
provisioning, where the operator's channel is the transport.

## Provider matrix

| | Builtin WireGuard | Tailscale (spec-proof) |
|---|---|---|
| control-plane addr | derived ULA IPv6 (`f(cluster_id, pubkey)`) | tailnet IP |
| container plane | per-machine IPv4 /24, door-allocated | same /24s, advertised as tailnet subnet routes (operator approves in their admin) |
| `bind_ip` | own derived v6 `::1` on `wg0` | tailnet IP on `tailscale0` |
| `provision_join` | keygen + derive v6; door allocates /24 | read local tailnet IP; door allocates /24 |
| `converge_peers` | roster → WG peer set | no-op — the tailnet owns peers |
| source integrity | cryptokey routing | the tailnet |
| laptop dial | userspace WG, no root (gotatun + smoltcp + hyper connector, per the userspace-WG research) | plain sockets |
| NAT'd ↔ NAT'd | refused; refusal names Tailscale | DERP relays (theirs) |
| join transport | public door, token + pinned fingerprint | same door reached over the tailnet |
| remove | sweep rows; Keepers drop the peer | sweep rows; parting message: "remove node from your tailnet admin" |
| control-plane driven by Ployz | yes (it is the product) | **never** — no API key, no ACLs; Ployz rides, never drives |

## The container plane

Fixed by the wayfinder ticket [Decide: container-plane wiring over the
per-machine /24](https://github.com/getployz/ployz/issues/801). Three
seams: the bridge, service DNS, and namespace isolation.

### The bridge: a Docker network owned by the API fold

Each machine runs one Docker bridge network named `ployz` — pinned Linux
bridge name and MTU, Docker IPAM over the machine's /24, gateway `.1`,
containers `.2+`. The **API fold** (the Docker-socket holder) creates it
one-shot at join; deploys require it and never create it.

The API fold also watches its **own** machines row: when `subnet_v4`
differs from the live network's subnet, it recreates the network and
restarts the machine's containers. That makes the API fold the executor
of Keeper's subnet self-heal — Keeper authors the row fix (its one named
row-law exception) and stays Docker-free per its charter; the fold
enforces the recorded decision locally. Keeper's mesh converge keeps
owning routes, allowed-ips, and the eBPF route map from roster rows.

### Service DNS: A records from rows as written

Every machine's DNS role answers for the whole cluster from its local
Corrosion — no cross-machine query path. The resolver binds the bridge
gateway (`.1:53`); containers are wired with `dns = .1`,
`search = <namespace>.internal`, `ndots:1`. A records only, for
`<service>.<namespace>.internal`, TTL 5s; non-`.internal` queries
forward to the host's upstream resolver.

Records come from `containers` rows — whose documents gain `ip` (bridge
IPv4) and `deploy` (revision) fields — joined to `services` rows and
filtered to `container.deploy == service.active_deploy`, so the
blue/green flip is atomic in DNS exactly as it is at the gateway.
Namespaces resolve via `namespaces` rows.

**No liveness filter.** Rows are served as written: a crashed (not
removed) machine's container IPs keep resolving until `machine rm`
sweeps its testimony. Liveness is WG handshake age at the point of use
(`status`/`doctor`), never inferred into answers — and the DNS role is
unprivileged, so it could not read handshake state without new
machinery. Named ceiling: a single-container service on a dead machine
resolves to a dead IP until repair.

### Namespace isolation: one cgroup_skb pair

v1 enforces exactly one fixed rule, with no operator surface:

> Drop a packet iff its source and destination are both in the
> container map and their namespaces differ.

Everything else passes untouched: host-sourced traffic (gateway
upstreams, DNS, health checks — `.1` is not in the map), internet
egress, and the control plane (IPv6-only, unreachable from pure-IPv4
containers by construction). Cross-machine flows are enforced at both
ends — the sender's egress hook and the receiver's ingress hook.

- **Mechanism:** one `cgroup_skb` ingress/egress program pair, attached
  once at the root cgroup with `BPF_F_ALLOW_MULTI`, link pinned in
  bpffs. It lives in the existing aya crate beside the routing program
  and is driven through `ployz-ebpf-ctl`. The bridge tcx program stays
  routing-only forever. The socket-level hook sees same-machine
  container↔container traffic that tc on the bridge structurally
  cannot (L2-switched between veth ports), with zero per-container
  attach churn.
- **The map:** `container_ip → namespace_id`, built cluster-wide from
  `containers` rows (the same `ip` field DNS reads). An unknown IP
  inside the container prefix drops — fail closed within the prefix; a
  freshly started remote container is dark for one gossip beat
  (sub-second), absorbed by connection retries.
- **Applied by Keeper** — and this is the priced-in charter amendment:
  Keeper's converge diet gains a **second named family**, `containers`
  rows, consumed solely for this map. It stays inside the one law
  (converging toward rows it does not own — container testimony is the
  API fold's), but the "exactly one family" line in the Keeper charter
  carries this recorded, scoped exception.
- **Requirements:** cgroup v2 unified hierarchy (any modern distro;
  OrbStack VMs included); kernel floor unchanged — `cgroup_skb` needs
  4.10, far below the tcx 6.6 floor already shipped.
- **Named ceilings, future notes:** the hook covers socket-originated
  traffic only — a container emitting raw/spoofed L2 frames bypasses
  it; the closer is an nftables bridge-family backstop, not built.
  Per-namespace knobs (an allow field between namespaces, or opt-out)
  are the additive path on the namespace row.

Shipping deny-by-default in v1 is deliberate: v2 is greenfield-only
(#788), so the wall goes up while nobody lives against it — deferral
would re-buy a non-additive retrofit on every cluster created meanwhile.

### The machine boundary rule

A Ployz machine is a **Linux environment**, wherever it runs — bare
metal, cloud VM, or an OrbStack Linux VM on a Mac. Every enforcement
tool above (kernel WG, the bridge, cgroup_skb, sysctls, systemd) is
Linux-only, and under OrbStack the containers run inside the Linux VM
anyway. macOS itself is only ever a `Peer` (the operator-laptop dial);
a Mac becomes a workload machine by running `ployzd` inside an OrbStack
Linux machine, which is, to Ployz, just a Linux machine.

### Considered and rejected (container plane)

- **Keeper-created raw Linux bridge**: Docker loses IPAM, and Keeper
  still cannot restart containers on heal — the Docker socket stays
  out of its charter. Worst of both.
- **Deploy-path lazy network creation**: the subnet self-heal would
  have no executor until the operator happens to deploy; 200
  unattended cloud-init joins is exactly the case that must heal alone.
- **DNS liveness filtering** (handshake age or health checks): infers
  liveness into served truth, needs privileged reads or a reconciler
  the charter killed. Dead machines are a visible repair.
- **Deferring isolation to post-v1**: re-buys the retrofit hazard
  greenfield currently makes free.
- **Configurable network policy** (k8s NetworkPolicy shape): an
  operator surface and a second product.
- **tc/tcx policy on the bridge device**: structurally blind to
  same-machine container↔container traffic; br_netfilter punts bridged
  frames to netfilter, never to tc.
- **Per-veth tcx (the Cilium pattern)**: complete coverage bought with
  per-container attach lifecycle and a fail-open race window.
- **`BPF_PROG_TYPE_NETFILTER`**: no aya support, kernel 6.4+, still
  needs br_netfilter — buys nothing over plain nftables here.
- **nftables as the primary policy layer**: works (Docker's own
  `icc=false` path), but is a second enforcement mechanism beside the
  aya stack the product already ships; demoted to the spoof backstop.

## Considered and rejected

- **IPv6-only containers** (full Fly-style 6PN): rejected on the Railway
  precedent — IPv6-only container networking breaks real workloads.
  IPv6 survives where nobody but Ployz looks.
- **Token-as-WG-identity join** (token embeds a pre-trusted private key,
  join runs over the mesh, zero public doors): elegant, but a multi-use
  token is one shared WG identity — 200 concurrent joiners flap one
  pubkey/IP between endpoints. Died on the cloud-init requirement.
- **Per-machine IPv4 as the control plane** (Uncloud's layout alone): a
  subnet heal would move the machine's own mesh IP mid-flight, breaking
  gossip and open streams. Dual-stack shrinks the heal to containers.
- **Driving Tailscale** (API key, ACL sync, node lifecycle): a tailnet
  manager smuggled into the provider — second system. Ployz rides.
- **Deterministic IPv4 subnets**: 65k /24s is birthday-collision
  territory at 200 machines; IPv4 is allocated, IPv6 is derived.
