# Engine exploration for E1..E4 (inspect only, nothing changed)

Paths are relative to `core/`. Test commands run from `core/`.

## Contradictions between the design and the code (read first)

1. **Doors no longer live on Caddy's row once the holder lands.** Decision 2 says "the ports already on the row say which door is open". With the pause holder, 80/443 move to the holder. Caddy's spec has no ports in both shapes. `IngressPorts::{Public,Closed}` must then be a property of the holder spec, and the holder must be observable. It cannot be a new `ContainerKind`. That enum is strict serde with no `#[serde(other)]` (`crates/ployz-core/src/domain/observation.rs:213-218`), so a new variant breaks older readers (Stable promise red flag). Pick one: a reserved Service row `ployz-system/ingress-holder` (kind `ServiceContainer`), or a daemon-local unmanaged container that doesn't replicate (then `ployz ingress` and Deploy evidence can't see the doors).
2. **"Older daemon narrows" is false, as /interrogate suspected.** `placement_eligibility_in_namespace` (`crates/ployz-core/src/service/eligibility.rs:46,67`) treats a non-canonical spec in `ployz-system` as an ordinary Service. `placement_eligibility` then checks `accepts_services`, not `accepts_ingress` (`:114-117`). An older CLI or catch-up would place a new-shape Caddy on any Machine that accepts services. That widens placement.
3. **Decisions 3, 4 and 6 still say `container:<local caddy id>` and "restart stranded connector".** The holder section at the end supersedes them. Fix the text before the PR bodies cite it.
4. **ACME is the daemon's, not Caddy's.** The Caddyfile renders `auto_https off` (`crates/ployzd/src/ingress/caddy.rs:242-251`). Issuance is `crates/ployzd/src/certificates.rs` (`issue_wanted` :425, `machine_rank` :57, `ingress_challenge_ips` :227). "Skip ACME when ports closed" means three things. A Closed Machine leaves the rank list (:470-473). It leaves `ingress_challenge_ips`. When no Machine has an open public door, the run records a refusal reason, as the policy-refusal path at :437-451 does.
5. **The trusted-proxy option is global today.** `trace::cloudflare_client_ip_servers` (`crates/ployzd/src/ingress/caddy/trace.rs:43-47`) writes one `servers { trusted_proxies … }` block for every listener. Loopback trust must be scoped to a `servers :8480 {}` block, or 127.0.0.1 becomes trusted on :80/:443 too. A test at `crates/ployzd/src/caddy_tests/trace.rs:357` already asserts that only `trusted_proxies` reads `CF-Connecting-IP`. Extend it.
6. **`caddy_service_spec` validates against the submitted image** (`crates/ployz-core/src/domain/ingress.rs:58-63,75-89`). That matches the audit finding. If the daemon pins a digest, the validators must compare against the daemon's pin. But Machines in a rolling upgrade carry different pins. Catch-up copies an observed slot spec, so it would carry a peer's pin (`crates/ployz/src/global_catch_up.rs` module doc :1-12), and the joiner's validator would refuse it. Decide this before E1. Option one: catch-up and `follow_roles` re-derive the spec locally (the daemon picks the image) instead of copying it. Option two: validators accept any digest in a small allow-list that ships with the release.
7. **The routes vs. catch-up mismatch for tunnel and tailscale.** Catch-up places reserved Globals with `EnsureContainer` from the observed spec (`global_catch_up.rs:164,566,752,831` special-case only `is_proxy`). If the tunnel and tailscale are reachable only through `SetIngressDoors` / `SetPrivateAccess` with a sealed token, catch-up on a joining Machine must call those routes. It has no other way to start them, and it needs the sealed token from a replicated observation (ciphertext, fine to read).
8. **The DNS parser can't see the Cluster Name.** `dns/query.rs::parse` is a pure fn of the name (`crates/ployzd/src/dns/query.rs:30-43`). Today `postgres.prod.acme.ployz.internal` has 4 labels and falls to `_ => Empty`, not NXDOMAIN (`:80`). E3 must thread the name in and add an NXDOMAIN outcome for a foreign cluster label.

## E1. Holder-netns Ingress Proxy

**Data shape.** `IngressPorts { Public, Closed }` lives on the holder spec. Caddy gets one canonical spec with `network: JoinHolder`, resolved at create to `container:<holder id>`. Both images are pinned by digest in a daemon constant.

Files:
- `crates/ployz-core/src/domain/ingress.rs:36-89`. `caddy_service_spec(constraints)` drops `image`. Add `holder_service_spec(ports, constraints)`. Validators return `IngressPorts`, not `()`. `caddy_ports()` (:115) moves to the holder. Add a loopback `:8480` in `caddy_container` only if Caddy needs an `expose` (it doesn't, since the netns is shared).
- `crates/ployz-core/src/domain/spec.rs:472-514`. `ServiceContainerSpec` needs a network-join field. The precedent is `pid_mode: Option<PidMode>` with `PidMode::Container(String)` (`crates/ployz-core/src/host_config.rs:110`). Mark it `#[serde(default)]`. An older reader ignores it, which is why point 2 matters.
- `crates/ployzd/src/docker/create.rs:161`. `network_mode` is hard-coded to the `ployz` bridge. Branch it, and skip `dns`, `dns_search`, `port_bindings` and `extra_hosts`, since Docker rejects them with `container:` mode.
- `crates/ployzd/src/docker/lifecycle.rs:819-840` (`create_container` reserves a bridge address only when `on_bridge`; fine). `:981` (`lost_ployz_network`) returns false for `container:` mode, which is correct. Check the `:540,553` rejoin path so it never calls `join_bridge` on Caddy.
- `crates/ployzd/src/docker/observe.rs:570` `stranded_ingress` filters by identity. If the holder is `ployz-system/ingress-holder`, it needs its own stranded case: start the holder first, then Caddy. Docker refuses to start Caddy while the holder is down. The comment at `:364-368` and the bet 10 sentence ("only Service Container the daemon starts unasked") widen to "the Ingress Proxy's two containers".
- `crates/ployzd/src/ingress.rs:417-431` `newest_local_ingress` and `serves_nothing` (:391) match on identity. Keep the holder out of them by giving it a distinct identity.
- `crates/ployzd/src/machine/ingress.rs:12-22` `admit_ingress_service` admits both identities.
- `crates/ployzd/src/ingress/caddy.rs:233-300` `render_caddyfile` adds the `http://:8480` site block with `bind 127.0.0.1 ::1`, plus the scoped `servers :8480` trust (point 5). The verify handlers answer with `Ployz-Door: tunnel` (`verify_handle` :486).
- `crates/ployz/src/ingress.rs:32-60,344-396`. `service_spec`, `IngressImage::{Keep,Latest,Given}` and `desired` collapse, since the daemon picks the image. `crates/ployz/src/ingress/caddy.rs` (`latest_image`, `pinned_image`, the `--ingress-image` parse) is deleted, along with its CLI flag and docs. `follow_roles` (:67) and `slot_repairs` (:209) must deploy two slots, holder before Caddy.
- `crates/ployz/src/deploy/apply.rs:17` `apply_requested` handles one spec, so ordering holder→Caddy needs either two calls or a single reserved route (see E2: `SetIngressDoors` could own both from E1 on).
- `retire_ingress_proxy` in `crates/ployz/src/global_slot/lifecycle.rs` stops Caddy, then the holder. `global_catch_up.rs:511` `is_proxy` covers the holder.
- `crates/ployzd/src/certificates.rs` (point 4) can land in E1 or E2.

Nearest tests:
- `crates/ployz-core/src/domain/ingress.rs` tests (`caddy_wiring_round_trips_through_both_validators`): `cargo test -p ployz-core --lib domain::ingress` (rung 1).
- `crates/ployz/src/ingress.rs:610-676` (`builds_the_caddy_service_spec`, `desired` tests): `cargo test -p ployz --lib ingress::tests`.
- `crates/ployzd/src/docker/observe.rs:633-700` (stranded): `cargo test -p ployzd --lib docker::observe::tests`.
- `crates/ployzd/src/docker/lifecycle.rs:1002-1030,1541`: `cargo test -p ployzd --lib docker::lifecycle`.
- `crates/ployzd/src/docker/mod.rs:1981-2004` (pid_mode `container:` precedent): `cargo test -p ployzd --lib docker::tests`.
- Caddyfile: `cargo test -p ployzd --lib caddy_tests` and `--lib ingress_tests`.
- `crates/ployz/src/global_catch_up_tests.rs:548` `a_proxy_copy_skips_rotation`: `cargo test -p ployz --lib global_catch_up`.
- Rung 4 (also run the `verify-caddy-load` model check): `crates/ployz/tests/ingress_cluster.rs`, listed in `scripts/run-layer3-tests.sh:62`. `PLOYZ_TESTKIT_IMAGE=$(scripts/testkit-image.sh ployz-testkit-checkout:dev) cargo test -p ployz --test ingress_cluster -- --ignored --test-threads=1`.

## E2. SetIngressDoors and the tunnel

Adding a route follows `ReadRoutedAddresses` across these places:
1. A catalog row in `crates/ployz-core/src/rpc_catalog.rs:49-93`: `SetIngressDoors: (set_ingress_doors, "SetIngressDoors", SetIngressDoorsRequest, "set_ingress_doors", IngressDoorsSet, SET_INGRESS_DOORS_CAPABILITY, "ployz.ingress.doors.v1", Container, Operator)`. Scope is `Operator` now. Signing comes later.
2. Request and response types in `crates/ployz-core/src/rpc.rs` (near :1457), with `#[serde(default)]` on every optional field.
3. The daemon handler `crates/ployzd/src/machine_api/local.rs` (pattern at :971) and tests in `machine_api/local_tests.rs` (pattern `mod read_routed_addresses` :3211).
4. Every `MachineApi` test double must implement it. They live at `crates/ployz/tests/cloud_init/harness.rs:975`, `tests/deploy_client/support.rs:1491` and `tests/connect/support.rs:1367`, plus `crates/ployz-testkit/src/fake_machine.rs` and `fake_fleet.rs` (they consume `rpc_catalog!`).
5. The capability list test `crates/ployz-core/tests/capabilities.rs:13,107`: `cargo test -p ployz-core --test capabilities`.
6. Cloud: a `PloyzSession` method in `crates/ployz/src/sdk.rs` (pattern `start_ingress` :1088), exposed through napi in `crates/ployz-sdk/src/lib.rs`. Then `pnpm sdk` in `dashboard/`, which runs `core/scripts/build-cloud-sdk.sh`. Nothing generated is committed.

Other files:
- The sealing context. `SealedIdentity` is `deny_unknown_fields` and bound to Org/Environment/Service (`crates/ployz-core/src/sealed.rs:70-79`, AAD :130). Add a sibling platform context with its own info string and a purpose of `ingress-tunnel`. The opening side goes next to `crates/ployzd/src/sealed_env.rs`. The tmpfs mount is already supported through `RawVolumeSource::Tmpfs` (`crates/ployzd/src/docker/create.rs:471`). Writing the opened token into it needs a daemon step between create and start, which exists only for Build secrets today (`BUILD_SEALED_CAPABILITY`). Find that path and reuse it.
- `QualifiedService::system_tunnel()` goes next to `system_ingress()` (`crates/ployz-core/src/value.rs:735`). A `tunnel_service_spec()` goes in a new `crates/ployz-core/src/domain/tunnel.rs`.
- CLI: new `crates/ployz/src/handlers/ingress.rs`, wired in `crates/ployz/src/cli/mod.rs` and the handler table. `--json` per CODING_STANDARDS. Per-Server `DoorStep` rows finish through `ui::finish_fanout`.
- `confirm_ingress_off` (`crates/ployz/src/handlers/server.rs:958-990`) keys "last" on `accepts_ingress && public_ip.is_some()`. Change it to "has an open door" (public-door Machine, or tunnel observed running). Its tests are at :2383 and :2414: `cargo test -p ployz --lib handlers::server`.
- Rung 4: `certificates_cluster` (`scripts/run-layer3-tests.sh:87`) for the ACME skip. Same command shape as above with `--test certificates_cluster`.

## E3. Cluster Name and the DNS alias

- `Namespace::is_reserved` (`crates/ployz-core/src/value.rs:656`). Add `ployz`. The test is `crates/ployz-core/tests/contract.rs:247`: `cargo test -p ployz-core --test contract`. Callers that filter reserved Namespaces (`drain.rs:59,614`, `cluster_teardown.rs:55,118`, `deploy/planning.rs:1325,1346`, `ployz-store/src/deployment/mod.rs:1565`, `namespace.rs:21`) inherit the change. `ployz-store`'s refusal means Cloud can't create Namespace `ployz`, so run `cargo test -p ployz-store` filtered to deployment.
- The Cluster Name newtype goes in `ployz-core/src/value.rs` (63-char DNS label, CONTEXT term). It is stored as an optional `cluster` key `name`. The write and read follow `sealing_public_key` (`crates/ployzd/src/corrosion/store.rs:158-166`, fake at `corrosion/fake_cluster.rs:476,543`). Writing it needs a route, `SetClusterName` / `ployz.cluster.name.v1`, using the E2 recipe.
- DNS: `crates/ployzd/src/dns/query.rs:30-82`. `parse(name, cluster_name: Option<&ClusterName>)`. Strip `.<cluster>.ployz.internal.` and reuse `parse_internal`. Add `Query::NxDomain` for a foreign or unset label. Map it in `dns/service.rs`. Tests are in `query.rs:97-201` and `dns/tests.rs`: `cargo test -p ployzd --lib dns`. Rung 4 is `internal_dns_cluster` (`run-layer3-tests.sh:57`).
- Founding: the commands are `server add --standalone|--token-stdin` (`crates/ployz/src/handlers/mod.rs:1349`). Add `--cluster-name`. Per CODING_STANDARDS, `--json` never prompts, so it fails with `invalid_argument` naming the flag. Add the `ployz cluster name` verb. Shape tests: `cli_shape.rs`.

## E4. Reserved tailscale

- `QualifiedService::system_tailscale()`, a canonical spec with `ServiceMode::Global` and a pinned digest, and a sealed auth key in its own purpose context (E2 machinery).
- The `SetPrivateAccess` route (`ployz.private-access.v1`) uses the E2 recipe.
- Placement: Global on every Machine, gated on nothing (not `accepts_ingress`). `placement_eligibility` (`eligibility.rs:107-117`) has only ingress/services branches. A reserved non-ingress Global falls to `accepts_services`, which leaves out ingress-only Machines. Add a reserved branch.
- Catch-up: `global_catch_up.rs:705` `catch_up_globals` plus point 7.

## Docs and CONTEXT

- `core/CONTEXT.md:544` Ingress Proxy. It becomes "a holder container that owns the network namespace and published ports, plus Caddy". Add terms for Door, Tunnel Connector, Cluster Name, Private Access, and Platform sealing purpose. `:151` Namespace: reserve `ployz`. `:195` Global catch-up: reserved Globals.
- `core/DESIGN.md` bet 10 sentence ("only Service Container the daemon starts unasked") if the holder is started unasked.
- `docs/user/servers/add-a-server.md:324-335`. "Ployz opens the ports it needs" plus the ports table, split per door.
- `docs/user/services/domains.md:136,162` ("via proxy", "Behind Cloudflare?") add "via tunnel".
- `docs/user/troubleshooting/app-not-reachable.md` gets a tunnel section. New `docs/user/services/cloudflare-tunnel.md`.
- `docs/user/services/private-networking.md:6,45` gets the `<cluster>.ployz.internal` alias and Tailscale.

## verify-server lanes

Launch with `run=$(scripts/verify-cluster up --machines 2)` (Boat, checkout daemon, Standalone), then `doctor "$run"`. Use `--cloud` for the product-path proof. The bake pre-pulls only `caddy:2` (`scripts/verify/prepare-guest.sh:10`). The pause, cloudflared and tailscale digests pull at first create, so time-sensitive checks should pre-pull them with `exec`.

- **E1.** Public-door regression: deploy `whoami`, then curl :80 and :443 on both Machines. Caddy crash: `exec "$run" machine-1 -- docker kill -s KILL <caddy>`, then check the holder survives, Caddy restarts in place, and ports stay published (`ss -ltn`). Caddy replace: an upgrade path through `server set --ingress off/on`. Stranded start: `exec … systemctl stop ployz; docker stop <holder> <caddy>; systemctl start ployz`. `power off/on` covers boot.
- **E2.** "Ports closed" is directly testable: `cli -- ingress use tunnel --token-stdin`, then `exec … ss -ltn` shows no :80/:443, and `docker inspect <holder>` shows no PortBindings. Tunnel serving needs a real Cloudflare tunnel token. That is a free Cloudflare account, but it is an external action, so it is outside this brief's constraints. The loopback listener is checkable without it: `exec … docker run --rm --network container:<holder> curlimages/curl http://127.0.0.1:8480 -H 'Host: …'`.
- **E3.** `exec "$run" machine-1 -- dig @<gateway> postgres.<env>.<cluster>.ployz.internal`. `dns-soak` with `--name` works for regressions.
- **E4.** Needs a tailnet auth key (external, out of scope). The mechanics run as before, as a user Service.
- **Global catch-up on a joining Machine is a gap.** `verify-cluster` has no "add a Machine to an existing run" action (subcommands at `scripts/verify-cluster:969-1000`). `up --machines 2` enrolls both up front. A third Machine would need a manual `server add` through `exec`/`cli` against a separately booted `--bare` host, an unproven workflow. Building an `add` action is the lever.

## Summary

1. Biggest gap: after the holder graft, the door state lives on the holder. It needs a replicated identity (a new reserved Service, not a new `ContainerKind`) or the design's "ports on the row" claim fails.
2. Older daemons widen, not narrow. A non-canonical `ployz-system/ingress` falls back to `accepts_services` (`eligibility.rs:114-117`).
3. Pinned digests clash with catch-up copying observed specs during rolling upgrades. Re-derive specs locally or allow-list digests.
4. ACME is daemon-side (`certificates.rs`), and the Caddyfile's trust block is global. Both need scoped changes, not a Caddy flag.
5. Each new route follows the `ReadRoutedAddresses` path: catalog row, `rpc.rs`, `local.rs`, five test doubles, `capabilities.rs`, `sdk.rs`, napi, `pnpm sdk`. Verify-cluster covers closed ports and Caddy crash. It can't yet add a joining Machine.
