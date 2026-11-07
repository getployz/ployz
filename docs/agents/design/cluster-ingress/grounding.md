# Grounding: cluster ingress modes (Caddy public vs BYO Cloudflare Tunnel)

Repo: /home/codex/.t3/worktrees/ployz-rust/t3-2336df24. Terms per core/CONTEXT.md and dashboard/CONTEXT.md. Bets in core/DESIGN.md.

## Goal

Two cluster ingress modes:
- (a) Public: as today. Caddy (Ingress Proxy) publishes 80/443 on every ingress Machine, DNS points at public IPs, ACME HTTP-01 or Cloud-published Certificate Material.
- (b) Tunnel: cloudflared runs as a Global Service, pointed at the Machine-local Caddy. No public HTTP ports. **BYO Cloudflare only** (decided by the user): a named tunnel in the user's own Cloudflare account, hostnames in the user's own Cloudflare zone. Ployz.app / Hosted DNS / quick tunnels are out of scope for tunnel mode.

Explicitly not decided yet: everything else (where the mode lives, who creates the tunnel, how hostnames are routed, how Caddy changes).

## Today (from three read-only explorers, file:line cited)

### Ingress Proxy
- `caddy_service_spec` (core/crates/ployz-core/src/domain/ingress.rs:36-132): reserved Global Service `ployz-system/ingress`, `PortPublication::Host { bind: All }` for 80/tcp, 443/tcp, 443/udp. Admin over unix socket. Only image and placement constraints vary. `validate_requested_ingress_service_spec` / `validate_ingress_service_spec` (ingress.rs:58,75) require exact equality with the canonical spec; only a spec passing them "is the Ingress Proxy".
- Runs on Docker network "ployz" (bridge), not host network (ployzd/src/docker/create.rs:162). Ports published by Docker DNAT, which bypasses INPUT and `PLOYZ-INPUT`.
- Role gate: `placement_eligibility` (ployz-core/src/service/eligibility.rs:46,102): `accepts_ingress` only for the exact Caddy spec in ployz-system; everything else (even other ployz-system Services) gated by `accepts_services`.
- `ployz/src/ingress.rs` `follow_roles`/`wanted`/`fill_slots`: deploys Caddy to exactly the Machines with `accepts_ingress` (CLI `server set --accepts-ingress`, Cloud `startIngress`/`stopIngress`, founder enroll handlers/cloud.rs:624-695). `apply_requested` bypasses `refuse_reserved` (ployz/src/namespace.rs:20), which blocks user deploys into ployz-system.
- Global catch-up (ployz/src/global_catch_up.rs) copies newest observed spec of each eligible Global onto a joiner; no ordering between Globals. Globals pinned (`StayReason::Global`).
- Caddy is never a Serving Container (`ServingContainer::prove`, service/serving.rs:20,80), so no Internal DNS name for it. A container can reach it by Caddy's Container Address on the ployz bridge, or via `host-gateway` + published port (extra_hosts supports host-gateway, container_metadata.rs:147).

### Projection and Caddyfile (ployzd/src/ingress.rs, ingress/caddy.rs)
- `IngressProjection::derive` -> per Ingress Hostname: owner, endpoints (Container Address:port), certificate (challenge, material, last_error).
- Caddyfile: `auto_https off`; catch-all `http://` answers verify paths with Machine id; catch-all `https://` -> 404 NOT_DEPLOYED; per-host `http://host` proxies OR 308-redirects to https when https-only and material exists; `https://host` site rendered **only when material exists**.
- trace.rs:42: `trusted_proxies static <Cloudflare ranges>` + `client_ip_headers CF-Connecting-IP`. Trust is keyed on peer address.

### Certificates
- ployzd/src/certificates.rs: HTTP-01 only, every daemon by Machine rank. `hostname_verdict` (ployz-core/src/domain/hostname_verdict.rs) probes `http://<resolved>:80/.well-known/acme-challenge/ployz-verify`; Machine-id body from a cluster public IP = Direct, from elsewhere = ViaProxy. `issuance_gate` (domain/issuance.rs:69). Refusal reasons advise "Open port 80" / "Point it at <public IPs>".
- Certificate Material (published by operator/Cloud) is served as given; ACME never touches it; a wildcard `*.x` covers one label. `certificate_policy` is a Corrosion `cluster` key read by the daemon (domain/certificate_policy.rs; capability `ployz.certificates.policy.v1`), no non-test writer found.

### Cloud (dashboard/)
- Cluster Domain (Hosted DNS, `*.name.ployz.app`): `cluster-domain/sync.server.ts` probes `http://<public_ip>/.ployz-verify` on ingress Servers with a public IP, PUTs apex A/AAAA (`hosted-dns.server.ts` `putHostedDomainRecords` takes only a/aaaa), gets a wildcard cert via Hosted DNS CSR, publishes it as Certificate Material. Managed hostnames are expanded against the Cluster Domain before reaching the runtime.
- Web traffic toggle per Server (`machines/server-web-traffic.server.ts:176`): updateMachine(accepts_ingress) then startIngress/stopIngress; Cloud stores no desired role, reads back observation. "Last Server with a public IP that takes web traffic" needs Approval (CLI handlers/server.rs:957 `confirm_ingress_off`, Cloud `webTrafficReach`).
- `PloyzSession` (runtime/ployz.server.ts:87-130) = Cloud's only path to Machines (Machine RPC via @ployz/sdk). Cloud never writes Corrosion directly.
- No Cluster- or Organization-level ingress setting exists. Cluster-wide replicated settings live in the Corrosion `cluster` k/v table (`network`, `sealing_public_key`, `certificate_policy`, ...). Per-Machine: `machines.info` (roles, labels).
- Unconfirmed: Cloud's Config Store may not express `mode: global` for user Services.

### Firewall and ports (ployzd/src/network/firewall.rs, linux.rs)
- No default-deny. `PLOYZ-INPUT` ACCEPTs UDP 51820 (WireGuard) and UDP 7573 (iroh management) from anywhere; locks 7569/7570/7572/7574 to the mesh. Re-asserted every 60 s, retired at teardown. Inputs: only MachineSubnet and ManagementAddress; no per-Cluster/per-Machine setting.
- 80/443 come from Docker publishing for Caddy; closing them = changing Caddy's port publications (or DOCKER-USER rules).
- 51820/udp stays needed between Machines (no WireGuard relay). 7573/udp closable (iroh falls back to outbound HTTPS relay). User Services can publish `Host` ports with bind All; nothing restricts that.
- Docs (docs/user/servers/add-a-server.md) imply Ployz guards the ports it opens; 80/443 actually bypass INPUT/ufw.

### Bets that bite (core/DESIGN.md)
- 9: Caddy is the only Ingress Proxy (cloudflared must front Caddy, not replace it).
- 10: The Ingress Proxy is the only Service Container the daemon starts unasked; daemon behavior needs one of three reasons.
- 3: no reconcilers / persisted desired state; Machine-local loops only for own serving infra.
- 7: Cloud drives, never owns; Cluster must stay operable without Cloud; a verb lives in `ployz` if a cloudless Cluster needs it.
- 6: Machine-local authority. 2: AP. 8: evidence over claims. Stable promise: replicated rows only gain optional fields, capability-named RPC, no version gates.

## Prototype results (measured 2026-11-06 on a real named tunnel in a throwaway Cloudflare zone, two Docker "Machines", each = Caddy + cloudflared on its own bridge, Caddy aliased `ingress`; all resources deleted)

1. **One remote config per tunnel, shared by all connectors.** Both connectors received the identical ingress config. So the origin URL (e.g. `https://ingress:443`) must resolve to the *local* Caddy on every Machine under the same name. A Machine-local stable name (network alias) worked in the prototype.
2. **No load balancing across connectors.** 30/30 requests went to connector m1 (both in the same Cloudflare colos). Replicas give availability, not spread. Caddy's own upstream balancing across the mesh still spreads load to Serving Containers.
3. **Dead origin behind a live connector = 100% 502, no failover.** Stopping Caddy on m1 while its cloudflared stayed up: 20/20 requests 502. Cloudflare does not health-check the origin. cloudflared must not outlive its local Caddy (lifecycle coupling, stop ordering).
4. **Killed connector fails over fast.** SIGKILL of cloudflared m1: 1 failed request (5 s timeout), then 39/39 served by m2.
5. **http origin + Caddy https-only redirect = redirect loop.** Tunnel to `http://ingress:80`: 308 to https, 5 redirects, never resolves.
6. **https origin works** with `noTLSVerify` + `matchSNItoHost` against a self-signed cert. Note: today Caddy renders `https://host` only when Certificate Material exists, else the catch-all 404 NOT_DEPLOYED.
7. **Client IP:** with `trusted_proxies` including the bridge range and `client_ip_headers CF-Connecting-IP`, Caddy's client_ip = real visitor IP; XFF and proto=https correct. Today's static Cloudflare-ranges trust would instead record cloudflared's container IP.
8. **Two-level wildcard fails at the edge on a free zone**: `*.sub.zone` TLS handshake failure. `*.zone` (one level, Universal SSL) or explicit hostnames work; deeper needs Advanced Certificate Manager.
9. **Quick tunnels unusable**: each cloudflared process gets its own random trycloudflare hostname; one hostname per tunnel.
10. Deleting a tunnel is refused while connections are live (error 1022); stale connections must be cleaned first. Relevant to teardown/mode-off.

## Open questions the design must answer
- Where the mode lives (Corrosion `cluster` key? per-Machine? Cloud-only? a reserved second Global spec?) and who writes it, cloudless included.
- Who creates the tunnel, its config and DNS: Cloud with the user's Cloudflare API token, the CLI, or the user by hand (token only)?
- How the tunnel token reaches Machines (Sealed value? secret in cluster state?).
- cloudflared as reserved platform Global (ployz-system, like Caddy) vs user Service; eligibility (ingress role vs services role); lifecycle coupling to Caddy.
- How Caddy reaches TLS-or-not in tunnel mode: (i) Caddy serves tunnel hostnames over plain http with no redirect (edge does TLS), or (ii) Cloudflare Origin CA cert published as Certificate Material and tunnel targets https, or (iii) other.
- What happens to ACME, Hostname Verdict, Cluster Domain sync, "last Server with a public IP" approval, port probe, user Host port publications in tunnel mode.
- Hostname routing at Cloudflare: one wildcard `*.zone` rule -> Caddy, or per-hostname rules/DNS synced from Ingress Hostnames.
- Mode switching on a live Cluster (public -> tunnel -> public) under bets 3, 4, 7.
