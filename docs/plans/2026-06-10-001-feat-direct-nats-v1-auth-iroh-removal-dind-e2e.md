---
title: "feat: Direct NATS v1 — auth, iroh removal, DinD e2e"
type: feat
status: planned
date: 2026-06-10
origin:
  - docs/adr/0013-v1-uses-direct-tls-nats.md
  - docs/adr/0014-keeper-update-is-separate-from-substrate-update.md
  - docs/plans/2026-06-04-001-refactor-nats-greenfield-control-plane-plan.md
  - AGENTS.md
---

# feat: Direct NATS v1 — auth, iroh removal, DinD e2e

Status: planned. Governed by [`ADR-0013`](../adr/0013-v1-uses-direct-tls-nats.md):
v1 machine connectivity is a direct TLS-authenticated NATS connection. All
iroh/tunnel machinery leaves the v1 path. NATS credentials plus server-side
subject permissions become the authority boundary. Hetzner acceptance is out
of scope; a Docker-in-Docker e2e harness proves multi-machine direct-NATS
clusters locally.

## Summary

Three phases, in order:

- **Phase A — Remove iroh from the v1 path.** Delete the tunnel runtime, the
  `Tunnel` daemon role, all `PLOYZ_TUNNEL_*` config, the iroh join-bundle
  material, the `bootstrap-tunnel` CLI surface, and the `iroh` +
  `ployz-transport` dependency edges. `ployz-transport` stays in the workspace
  as a doc-only stub ("future transport adapters", per AGENTS.md crate shape
  and ADR-0013's "possible future option").
- **Phase B — Real NATS auth.** One concrete scheme: cluster CA TLS on the
  wire + config-file-rendered per-machine **NKey users** with server-side
  subject permissions rendered from `NatsPermissionProfile`. First-node keeper
  install generates the cluster CA, the server cert, and the
  controller/operator/join NKeys. `machine add` returns its operation id
  quickly, then mints a per-machine NKey as bounded operation work: append its
  public key + Node permissions to a ployzd-owned `authorized-users.conf`
  (single-writer, never-shrinking), hot-reload `nats-server`, verify, and
  deliver the seed in `MachineJoinSecretDelivery.nats_credentials` at
  redeem time. The CA cert (public) rides in the join bundle, replacing both
  `core_iroh` and the `config_sha256` pin. Request-reply inboxes are
  per-principal prefixes, so the low-privilege Join credential cannot sniff
  another client's redeem reply. Gateway/DNS authenticate as the machine's
  Node user in v1 — no dedicated Gateway principal exists (a principal with
  no minting path would be a representable-invalid state). JWT operator mode
  is deliberately rejected for v1 (operator-key custody, resolver infra,
  audit opacity — wrong trade for 1–200 machines in one account with
  machine-add already an operation).
- **Phase C — DinD e2e harness.** A Rust harness (modelled on uncloud's
  `ucind`, reusing the proven recipe from `scripts/local-dataplane-proof.sh`)
  boots privileged systemd machine containers, installs the real keeper
  artifacts, forms a real TLS-authenticated cluster through product commands
  only, and asserts operations + running workloads + daemon-restart
  invisibility + auth rejection.

Auth posture rule: the listener becomes externally reachable **only** in the
same step that turns on TLS + authorization (Phase B). Phase A removes the
tunnel but every rendered config stays loopback until B3 — the current
`sed`-to-`0.0.0.0` hack in the proof script is exactly the failure mode B
eliminates.

## Committed Auth Scheme (Phase B contract)

- **Wire:** `nats-server` config gains `tls { cert_file, key_file }`. Cluster
  CA is self-signed, generated once at first-node install. Server cert SANs
  cover `127.0.0.1`, the node public IP/hostname (`--node-public-ip`), and the
  machine hostname.
- **Identity:** every NATS principal is an NKey user in a config-file
  `authorization { users [...] }` block. Public keys + permissions live in the
  config (non-secret, readable recovery evidence); seeds live in
  `0600` files on the machine that owns them.
- **Principals:** `Controller` (core ployzd control), `Node{node_id}` (one per
  machine, minted at machine-add / first-node activate), `User` (ployzctl
  operator), `Join` (shared low-privilege bootstrap user: publish only
  `plz.v1.svc.api.machine.join.redeem|report`, subscribe only its own inbox
  prefix), `System` (`$SYS.>`). **There is no Gateway principal in v1:**
  gateway and DNS processes authenticate as their machine's `Node{node_id}`
  user, and the Node profile carries the read-only route-state KV watch
  subjects they need. This is documented in the profile and revisited only if
  a dedicated gateway credential gains a real minting path.
- **Inbox isolation (no inbox sniffing):** each principal connects with a
  custom inbox prefix (`async-nats` `ConnectOptions::custom_inbox_prefix`,
  available in 0.49 — verified): `_INBOX_ctl` (Controller), `_INBOX_user`
  (User), `_INBOX_join` (Join), `_INBOX_node_<node_id>` (Node),
  `_INBOX_sys` (System). Each profile's subscribe allow is scoped to its own
  prefix (`_INBOX_join.>` etc.) — **no profile subscribes `_INBOX.>`**. This
  closes the hole where the deliberately weak Join credential (printed into
  install command lines) could subscribe `_INBOX.>` and intercept
  `machine.join.redeem` replies carrying minted per-machine seeds. The prefix
  is derived from the principal inside `NatsConnectConfig` (B2), so
  connecting with a mismatched prefix is unrepresentable.
- **Config ownership split (resolves the ADR-0014 intersection):** keeper owns
  `/etc/nats/nats-server.conf` (base config, written once at install, contains
  `include "authorized-users.conf"`). ployzd control owns
  `/etc/nats/authorized-users.conf` and rewrites + reloads it during
  machine-add. Keeper creates `/var/lib/ployz/nats/` at install and writes
  `ca.pem`, `server.crt`, `server.key`, `controller.seed`, `operator.seed`,
  `join.seed`; ployzd control writes `node.seed` there at activate-first-node
  (see B3 sequencing). The systemd unit gains
  `ExecReload=/bin/kill -HUP $MAINPID`; reload is non-disruptive.
- **Minting flow (bounded operation work, not handler work):** the
  machine-add handler validates, persists the operation, and returns the
  operation id + join token + join bundle quickly — it does **not** mint.
  Minting runs as bounded operation work after acceptance: generate node NKey
  keypair → upsert principal record into KV → render desired user set to
  `authorized-users.conf` → `systemctl reload nats-server` → verify with a
  bounded test-connect using the minted seed → store the seed as the
  per-machine `MachineJoinSecretDelivery`. Each step emits an operation event
  (`minted`, `rendered`, `reloaded`, `verified`, `material-ready`); each
  failure is a typed terminal failure with evidence. Reload failure is
  terminal, not a retry loop.
- **Authority-file durability (ADR-0001 classification):** the authorized
  principal set in `KV_CORE` is **explicitly named durable authority** whose
  recovery evidence is the on-disk `authorized-users.conf` (which survives
  JetStream loss). On control start and before any render, ployzd reads the
  existing file and adopts unknown entries into KV as observations. **Renders
  never shrink the user set** except as a step of an explicit machine-remove
  operation — after JetStream loss, an empty KV set must not overwrite the
  file and lock the fleet out. Background loops must not silently revoke
  credentials.
- **Render fencing (ADR-0015):** all render+reload work serializes through a
  single-writer owned task inside `nats_authorization.rs`; concurrent
  machine-adds queue their render requests rather than racing
  read-set→render→reload on the single file resource.
- **Trust distribution:** `MachineJoinTrustedNats` becomes
  `{ server_name, ca_pem }` (drop `config_sha256` — a per-machine-add mutable
  config invalidates a config hash by design). The joining keeper gets the CA
  + join seed from the `machine add` install command env (CA is public; join
  seed is deliberately low-privilege), then redeems the token over TLS and
  receives its per-machine seed in `secret_delivery`. The keeper retries
  redeem boundedly until the operation's `material-ready` event has landed or
  the token TTL expires.
- **Clients:** `async-nats` `ConnectOptions` with `require_tls(true)` +
  `add_root_certificates(ca)` + `with_nkey(seed)` +
  `custom_inbox_prefix(<principal prefix>)`. URLs use `tls://`.

## Out Of Scope

- Hetzner acceptance (`scripts/hetzner-two-node-acceptance.sh` is left stale
  with a "superseded by ADR-0013, do not run" header; its mirror test
  `crates/ployz-e2e/tests/h0_script.rs` is deleted).
- JWT operator mode, account multi-tenancy, cert rotation operations.
- Private overlay transport (iroh may return later via `ployz-transport`).
- WireGuard/eBPF dataplane changes (`wireguard_dataplane.rs` proof unchanged).

---

## Phase A — Remove iroh From The v1 Path

### A1. Delete the tunnel role and tunnel runtime

- **Goal:** `DaemonProcessRole::Tunnel`/`TunnelSide` and the entire ployzd
  tunnel runtime cease to exist; role sets shrink to control/node/gateway/dns.
- **Delete:**
  - `crates/ployzd/src/iroh_tunnel.rs` (whole file, 711 lines:
    `PreparedTunnelService`, `TunnelRestartPolicy`, `start_tunnel_runtime*`,
    `bind_iroh_endpoint`, `assure_tunnel_identity_file`, byte piping,
    `TunnelRuntimeError`).
  - `crates/ployzd/tests/iroh_nats_tunnel.rs` (whole file; its guarantee is
    re-proven by Phase C).
- **Rewrite:**
  - `crates/ployz-core/src/roles.rs`: remove `DaemonProcessRole::Tunnel(TunnelSide)`
    (line 14), `TunnelSide` enum (51–58), `process_name` arms
    `tunnel-edge`/`tunnel-core` (25–26), `command_args` arm (41–46),
    `parse_role_args` tunnel branches + `parse_tunnel_side` (194–210),
    `is_known_role` `tunnel` (214), error variants
    `MissingTunnelSide`/`UnknownTunnelSide` + Display (150–163), in-file
    tunnel tests. `first_node_process_set` becomes `[Control, Node,
    (Gateway)]`; `joined_node_process_set` becomes `[Node, (Gateway)]`.
  - `crates/ployzd/src/config.rs`: remove all 9 `PLOYZ_TUNNEL_*` env constants
    (35–43), `DEFAULT_TUNNEL_*`/`DEFAULT_CORE_TUNNEL_IROH_PORT` (44–46),
    `DaemonProcessConfig::Tunnel` variant, `load_tunnel_config` + helpers
    (274–436), `TunnelProcessConfig` (779–820), the 7 tunnel error variants +
    Display arms (489–518, 583–620), iroh/transport imports.
  - `crates/ployzd/src/app.rs`: remove `RoleProcessPlan::Tunnel`,
    `TunnelProcessPlan`, `TunnelWork`, `plan_tunnel_process`, the config match
    arm, imports.
  - `crates/ployzd/src/daemon_runtime.rs`: remove tunnel dispatch arm,
    `DaemonRuntimeError::Tunnel` + Display, import.
  - `crates/ployzd/src/main.rs`: remove `ployzd tunnel identity` subcommand
    (`parse_tunnel_identity_command`, `TunnelIdentityOutput`,
    `MainError::TunnelIdentity` + exit-code/Display arms).
  - `crates/ployzd/src/lib.rs`: drop `pub mod iroh_tunnel;` and the iroh doc
    sentence. `crates/ployzd/src/role.rs`: drop the `TunnelSide` re-export.
  - `crates/ployz-keeper/src/steps.rs`: remove local `PLOYZ_TUNNEL_*` /
    `DEFAULT_*TUNNEL*` constants (28–39), `PloyzdRoleEnvironmentTarget` tunnel
    fields + `with_core_tunnel_nats_addr`/`with_edge_tunnel` builders +
    `PLOYZ_TUNNEL_*` env render block (499–700), `PloyzdTunnelEnvironment`
    enum (705–712), default iroh bind helpers.
  - `crates/ployz-keeper/src/systemd.rs`: remove
    `ployzd-tunnel-{edge,core}.service` unit-name arms (344–345) and the
    `TunnelSide` import.
  - `crates/ployz-keeper/src/main.rs`: `keeper_join_target_with_public_ip`
    stops calling `MachineJoinEdgeTunnel::from_join_bundle`/`with_edge_tunnel`;
    joined-node role env renders `PLOYZ_NATS_URL` directly from
    `join_bundle.material.runtime_nats_url`.
  - `crates/ployzctl/src/commands/machine.rs`: delete
    `render_bootstrap_tunnel_command` (86–126) and the `bootstrap-tunnel`
    output line; machine-add output keeps operation/node/join-token/install
    lines only.
  - `crates/ployz-core/src/install.rs`: delete `MachineJoinEdgeTunnel` +
    `from_join_bundle` (197–220) — its only consumers die in this step.
  - Tests rewritten in lockstep: `crates/ployzd/tests/role_process.rs` (drop
    iroh/transport/tunnel imports and the 8 tunnel tests listed in the iroh
    map; keep control/node/gateway/dns tests — note: its
    `temp_join_template_file` fixture is reshaped in A2, not here),
    `crates/ployzctl/tests/cli_contract.rs` (5+ exact-string
    `supervise roles ...` assertions lose `tunnel-core`/`tunnel-edge`;
    bootstrap-tunnel output assertions deleted),
    `crates/ployz-keeper/tests/bootstrap.rs`, `tests/local.rs`,
    `tests/systemd.rs` (tunnel unit + `PLOYZ_TUNNEL_*` env expectations
    removed), `crates/ployz-core/tests/machine_lifecycle.rs` (process-set
    expectations).
- **Type/shape notes:** no replacement types. The role enum shrinks; invalid
  tunnel states become unrepresentable by deletion, per AGENTS.md.
- **Verification:** `cargo test --workspace` green;
  `grep -rn "TunnelSide\|PLOYZ_TUNNEL_\|tunnel-edge\|tunnel-core" crates/`
  returns nothing.

### A2. Slim the join bundle / install contract (serde schema change)

- **Goal:** the join bundle carries direct-NATS material only; all iroh
  newtypes and JSON fields die in one commit (both `MachineJoinBundle` and
  `MachineJoinMaterial` are `deny_unknown_fields`, so every embedded fixture
  must change in the same commit).
- **Rewrite:**
  - `crates/ployz-core/src/install.rs`: delete
    `MachineJoinCoreIrohEndpoint`, `MachineJoinIrohPublicKey`,
    `MachineJoinIrohDirectAddress`, `MachineJoinIrohRelayUrl`,
    `MachineJoinIrohTicket` (230–238, 344–509), `MachineJoinMaterial.core_iroh`
    field, `MachineJoinSecretDelivery.core_iroh_ticket` field,
    `InstallContractError::{InvalidIrohPublicKey, InvalidIrohDirectAddress,
    InvalidIrohRelayUrl, InvalidIrohTicket}` + Display arms.
    **Flip `MachineJoinRuntimeNatsUrl` semantics** (511–560): from "loopback
    tunnel listener" to "direct core NATS endpoint" — validation accepts
    `nats://`/`tls://` with hostname or IP host plus port (drop the
    `SocketAddr`-only `nats_url_socket_addr` requirement at 544; the
    `.socket_addr()` consumers were the tunnel listener and are gone).
    `trusted_nats`, `nats_credentials`, `secret_delivery`, artifact
    descriptors, and `KeeperFirstNodeInstall` keep their shapes in this step
    (reshaped further in B1/B4).
  - `crates/ployz-sdk-types/src/lib.rs` + `src/typescript.rs`: remove the five
    iroh re-exports/TS exports; `crates/ployz-sdk-types/tests/exports.rs`:
    rewrite the three JSON wire-format snapshots without
    `core_iroh`/`core_iroh_ticket`.
  - `crates/ployzctl/src/commands/init/join_template.rs`: remove
    `--core-iroh-public-key/--core-iroh-direct-address/--core-iroh-relay-url`
    parsing + fields + endpoint construction;
    `crates/ployzctl/src/commands.rs` usage text updated.
  - `crates/ployz-keeper/src/join.rs`: remove `JOIN_CORE_IROH_TICKET_FILE` and
    the four `core_iroh_*` redaction lines. `src/local.rs`:
    `commit_join_material_files` stops writing `core-iroh.ticket` (keeps
    `nats.creds`). `src/steps.rs`: remove
    `KeeperJoinMaterial.core_iroh_ticket` + `with_core_iroh_hints` +
    `RedactedJoinMaterial.core_iroh_*` fields.
  - Fixture sweeps (same commit): `crates/ployzd/tests/control_runtime.rs`
    (609–614, 655–656 + imports), **`crates/ployzd/tests/role_process.rs`
    (`temp_join_template_file`, ~717–768: embedded join-template JSON with
    `"core_iroh"` and `"core_iroh_ticket"` inside `"secret_delivery"`)**,
    `crates/ployzd/tests/backup_restore.rs` (268, 296),
    `crates/ployz-nats/tests/operations_nats/fixtures.rs`
    (153–158, 209–210) and `submission.rs` (141–142),
    `crates/ployzctl/tests/init_join_template_cli_contract.rs`,
    `tests/machine_add_binary_nats.rs`, `tests/api_client_nats.rs`,
    `tests/cli_contract.rs` (core_iroh fixtures),
    `crates/ployz-core/tests/install_contract.rs` (drop iroh validation tests;
    add tests for `tls://hostname:4222` acceptance and bad-scheme rejection),
    `crates/ployz-keeper/src/main.rs` + `tests/local.rs` fixtures,
    `scripts/local-dataplane-proof.sh` join-template heredoc (drop
    `"core_iroh"` at 227 and `"core_iroh_ticket"` at 255; drop
    `ployzd-tunnel-edge` from the diagnostics unit list at 308–309).
  - `crates/ployz-e2e/tests/support/nats.rs`: keep `TestNats` (1–69); delete
    `EdgeNatsTunnel` + `start_edge_nats_tunnel` (70–121) + iroh imports.
  - `crates/ployz-e2e/tests/operations.rs`: rewrite
    `e2e_edge_node_and_gateway_use_nats_over_iroh_tunnel` (897–1030) as
    `e2e_edge_node_and_gateway_use_direct_nats` (edge runtimes connect
    straight to `nats.url()`); rename the `op_e2e_iroh_*` ids; fix the
    join-template fixture (1266–1311).
- **Delete:** `crates/ployz-e2e/tests/h0_script.rs` (whole file — text-contract
  test pinned to the stale Hetzner script). Add a top-of-file comment in
  `scripts/hetzner-two-node-acceptance.sh`: stale, superseded by ADR-0013, do
  not run.
- **Verification:** `cargo test --workspace`;
  `grep -rn "core_iroh\|IrohTicket\|IrohPublicKey" crates/ scripts/` empty
  (except the quarantined hetzner script).

### A3. Drop dependencies, stub ployz-transport, rename readiness check, docs

- **Goal:** no iroh in the dependency graph; terminology matches CONTEXT.md.
- **Rewrite:**
  - `crates/ployzd/Cargo.toml`: remove `iroh = "1.0.0-rc.1"` (line 24) and
    `ployz-transport.workspace = true` (line 19).
  - `crates/ployz-e2e/Cargo.toml`: remove `ployz-transport.workspace = true`.
  - `crates/ployz-transport/src/lib.rs`: doc-comment-only stub ("future
    transport adapters if private connectivity returns"). **Delete**
    `src/iroh_endpoint.rs`, `src/nats_tunnel.rs`, `tests/nats_tunnel.rs`.
    Keep the crate as a workspace member (AGENTS.md crate shape).
  - `crates/ployz-core/src/machine.rs`: rename
    `MachineReadinessEvidence.nats_tunnel` → `nats_connection` (serde-visible;
    CONTEXT.md line 372 defines the term and says Avoid: Tunnel). Greenfield —
    no migration. Update `crates/ployz-core/tests/machine_lifecycle.rs:102`,
    `tests/operation_projection.rs:853`,
    `crates/ployz-sdk-types/src/typescript.rs` binding + `tests/exports.rs`
    snapshots.
  - `crates/ployz-core/src/lib.rs:7`: reword the "iroh endpoints" doc line to
    TLS NATS. `docs/architecture/machine-updates.md:132`: reword the
    "tunnel updates" out-of-scope bullet to "transport changes".
- **Verification:** `cargo tree -p ployzd | grep -i iroh` empty;
  `cargo tree --workspace -i iroh` errors with "not found";
  `cargo test --workspace && cargo clippy --workspace --all-targets`.

---

## Phase B — Real NATS Auth

### B1. Auth types and server-config rendering in ployz-core

- **Goal:** ployz-core owns the security role model and can render a complete
  TLS + authorization `nats-server` config; invalid auth states are
  unrepresentable; **the workspace compiles and is green at the end of this
  step** — every verified downstream consumer of the reshaped types is swept
  in the same commit.
- **Create:** `crates/ployz-core/src/permissions.rs` — move
  `NatsPermissionProfile` here from `crates/ployz-nats/src/permissions.rs`
  (AGENTS.md: ployz-core owns "security role models"; ployz-nats keeps a
  re-export for one release of the seam or callers update imports). Fix the
  known gaps while moving:
  - `Controller`: add publish allow `$JS.API.>` (bootstrap/KV/streams need it).
  - `Node{node_id}`: add the `$JS.API` consumer/KV subjects needed for the
    observation store writes and node-scoped KV reads; keep deny
    `$KV.KV_CORE.>` writes. **Also add the read-only route-state KV watch
    subjects** (plus the `$JS.API` read/consumer subjects they require):
    gateway and DNS authenticate as the machine's Node user in v1 — there is
    no Gateway principal (see Committed Auth Scheme).
  - New `Join` profile: publish allow exactly
    `plz.v1.svc.api.machine.join.redeem` and
    `plz.v1.svc.api.machine.join.report`; subscribe **only `_INBOX_join.>`**;
    `allow_responses`.
  - **Inbox scoping for every profile:** replace the shared
    `RESPONSE_INBOX = "_INBOX.>"` constant
    (`crates/ployz-nats/src/permissions.rs:12`, used by Controller/User/System
    at lines 71, 78, 84) with per-principal prefixes: `_INBOX_ctl.>`,
    `_INBOX_user.>`, `_INBOX_join.>`, `_INBOX_node_<node_id>.>`,
    `_INBOX_sys.>`. The prefix derivation lives next to the profile (one
    function `inbox_prefix(&NatsPrincipal) -> String`) so profile render and
    client connect (B2) cannot disagree.
  - Subject strings come from `crates/ployz-core/src/subjects.rs` constants —
    no new subject language.
- **Rewrite:**
  - `crates/ployz-core/src/security.rs`: `NatsPrincipal` gains a `Join`
    variant only. **No `Gateway` variant** — do not ship a principal with no
    minting path. Exhaustive matches across the workspace surface every
    render site — fix them all.
  - `crates/ployz-core/src/nats_config.rs`:
    - `NatsServerConfig` gains `listener: NatsListener` enum
      (`Loopback` | `External { advertise_host: NatsAdvertisedHost }`) — no
      stringly host field, no bool.
    - `tls: NatsServerTlsFiles { cert_file: PathBuf, key_file: PathBuf }`
      (required — a v1 config without TLS is unrepresentable; test servers
      render through the same type).
    - `authorized_users_include: PathBuf` and a separate renderer
      `render_authorized_users(&[NatsAuthorizedUser]) -> String` where
      `NatsAuthorizedUser { principal: NatsPrincipal, nkey_public:
      NatsUserPublicKey }` renders `{ nkey: U..., permissions {...} }` from
      the principal's `NatsPermissionProfile`.
    - New typed ids: `NatsUserPublicKey` (validated `U`-prefixed base32),
      `NatsUserSeed` (validated `SU`-prefix, `Debug` redacts),
      `NatsCaCertificatePem` (validated PEM block, public material).
    - Delete `trusted_nats_for_first_node`'s `config_sha256` derivation
      (72–82).
  - `crates/ployz-core/src/install.rs`: `MachineJoinTrustedNats` becomes
    `{ server_name: NatsServerName, ca_pem: NatsCaCertificatePem }`.
    `MachineJoinNatsCredentials` stays an opaque validated string (contents:
    NKey seed; documented; migration to `.creds` later is non-breaking).
    **`MachineJoinTemplate` keeps `secret_delivery` in this step** — dropping
    it before minting exists (B4) would leave `controllers.rs:328` with no
    secret source; the field drop moves to B4 where its replacement lands.
- **Same-commit consumer sweep (verified call sites that break on these
  reshapes — all updated here so B1 compiles):**
  - `crates/ployz-keeper/src/join.rs:85–90`: the redacted join-material
    render emits `trusted_nats_server=` / `trusted_nats_config_sha256=` lines
    — the sha256 line becomes a `trusted_nats_ca_sha256=` (digest of
    `ca_pem`, for human diffing) or is dropped; pick one and update the
    keeper-side parse.
  - `crates/ployz-keeper/src/steps.rs:186, 200–291`:
    `RedactedJoinMaterial.trusted_nats_config_sha256` field + constructors —
    reshape to match join.rs.
  - `crates/ployz-keeper/src/steps.rs:757`: `NatsServerConfig::single_node`
    call inside `NatsServerConfigTarget::for_first_node` — gains tls/listener
    arguments (B3 supplies the real material; here it takes the new required
    parameters with first-node defaults).
  - `crates/ployzd/src/nats_process.rs`: re-exports `NatsServerConfig`/
    `NatsServerConfigError` (line 6) and `PreparedNatsServerService::prepare`
    renders it (37–38) — update for the new required fields and validate the
    new paths in `validate_process_path`.
  - `crates/ployzd/tests/nats_bootstrap.rs`: 5+
    `NatsServerConfig::single_node` call sites (10, 24, 42, 63, 77) —
    updated for the new constructor shape; render-only assertions pass
    placeholder cert paths; any test that actually spawns `nats-server`
    against the rendered config moves its spawn coverage to B2 where the
    rcgen fixture exists.
  - `crates/ployz-core/tests/nats_config.rs` (existing test file): golden
    renders updated for `tls{}` + `include` + listener enum.
- **Tests:** `crates/ployz-core/tests/install_contract.rs` (trusted_nats
  reshape, ca_pem validation), `crates/ployz-core/tests/nats_config.rs` +
  new unit tests in `nats_config.rs` (golden render with `tls{}` + `include`,
  authorized-users render per principal, loopback vs external listener,
  per-principal inbox subjects), `crates/ployz-nats/tests/permissions.rs`
  moves/updates with the profile (including the no-`_INBOX.>` assertion).
- **Verification:** `cargo test --workspace` (the sweep above means the
  whole workspace, not just three crates, must be green at this boundary).

### B2. Authenticated connections in ployz-nats + secured test fixture

- **Goal:** every product connection is TLS + NKey with its principal's inbox
  prefix; anonymous plaintext is only constructible inside test fixtures.
- **Rewrite:** `crates/ployz-nats/src/connect.rs`:
  - New `NatsConnectConfig { url: NatsClientUrl, auth: NatsClientAuth,
    trust: NatsTlsTrust, principal: NatsPrincipal }` with
    `NatsClientAuth::NkeySeed(NatsUserSeed)` and
    `NatsTlsTrust::ClusterCa(PathBuf)` (single variants today; enums so the
    next credential form is a variant, not an option bag). The inbox prefix is
    **derived from `principal` via the B1 `inbox_prefix` function inside
    `connect_authenticated`** — a caller cannot construct a connection with a
    mismatched prefix.
  - `connect_authenticated(config, timeout)` builds
    `ConnectOptions::with_nkey(seed).require_tls(true)
    .add_root_certificates(ca).custom_inbox_prefix(inbox_prefix(&principal))`.
  - The existing bare `connect_with_timeout` moves behind the test fixture
    (or is `#[doc(hidden)]` for fixtures only); product callers must not
    reach it.
  - `crates/ployz-nats/Cargo.toml`: enable the async-nats 0.49 feature that
    pulls NKey auth support (`nkeys` is absent from Cargo.lock today with the
    current feature set — verify the exact feature name with
    `cargo tree -p async-nats -e features` and pin it).
    `custom_inbox_prefix` is in the 0.49 core API (verified), no extra
    feature.
- **Create:** secured NATS fixture consolidated into
  `crates/ployz-test-support/src/nats.rs` (new module; `lib.rs` gains
  `pub mod nats;`): `SecuredTestNats` generates a throwaway CA + server cert
  (dev-dependency `rcgen`) + NKey users for each principal (dev-dependency
  `nkeys`), renders the real `NatsServerConfig` (B1 types — the fixture
  renders through product code, no parallel config), spawns `nats-server`,
  exposes per-principal `NatsConnectConfig`s. Replace the duplicated
  plaintext `TestNats` copies in `crates/ployz-e2e/tests/support/nats.rs`,
  `crates/ployzd/tests/control_runtime.rs`, and
  `crates/ployzd/tests/wireguard_dataplane.rs` incrementally (full flip in
  B4/B5 — see the enumerated suite list there).
- **Tests:** new `crates/ployz-nats/tests/secured_connect.rs`: valid
  seed+CA connects over TLS; wrong seed rejected; valid Node seed publishing
  outside its allow list gets a permissions violation; plaintext connect to
  the TLS port fails; **inbox-isolation regression: a Join-credential client
  attempting to subscribe `_INBOX.>` (and another principal's prefix, e.g.
  `_INBOX_node_x.>`) receives a permission violation and observably cannot
  receive another client's request-reply traffic** (drive a Controller
  request-reply while the Join sniffer is subscribed; assert no delivery).
- **Verification:** `cargo test -p ployz-nats -p ployz-test-support`.

### B3. Keeper first-node install mints cluster identity and renders the secured server

- **Goal:** `ployzctl init --run-keeper-install` produces a running
  `nats-server` with TLS + authorization, plus on-disk credentials for
  Controller/User/Join — no `sed`, no post-install mutation. First-node
  Node-seed sequencing is explicit and typed.
- **Create:** `crates/ployz-keeper/src/nats_identity.rs`: generate cluster CA
  + server cert (deps: `rcgen` in ployz-keeper) with SANs
  `[127.0.0.1, <--node-public-ip>, <hostname>]`; generate Controller, User
  (operator), and Join NKey keypairs (dep: `nkeys`). Pure functions returning
  typed material (`ClusterNatsIdentity { ca, server_cert, controller,
  operator, join }`), no I/O.
- **Rewrite:**
  - `crates/ployz-keeper/src/steps.rs`: new `KeeperStep` variants
    `WriteNatsTlsMaterial` (→ `/var/lib/ployz/nats/{ca.pem,server.crt,server.key}`,
    key `0600`), `WriteNatsAuthorizedUsers` (initial
    `/etc/nats/authorized-users.conf` with Controller/User/Join users,
    written ployzd-writable per the ownership split),
    `WriteNatsClientCredentials` (→
    `/var/lib/ployz/nats/{controller.seed,operator.seed,join.seed}`, `0600`).
    `WriteNatsServerConfig` renders via B1 types: `listener: External` when
    `--node-public-ip` is given (this is the deliberate security flip — TLS +
    auth land in the same rendered config), `tls{}` block, `include
    "authorized-users.conf"`.
  - `crates/ployz-keeper/src/systemd.rs`: `nats-server.service` unit gains
    `ExecReload=/bin/kill -HUP $MAINPID`.
  - Role env rendering (`steps.rs`): every role env gets
    `PLOYZ_NATS_URL=tls://...` and
    `PLOYZ_NATS_CA_FILE=/var/lib/ployz/nats/ca.pem`. Seed paths are
    role-specific: control → `/var/lib/ployz/nats/controller.seed`;
    **node and gateway → the fixed path `/var/lib/ployz/nats/node.seed`** —
    which does **not exist yet at install time**. There is no controller-seed
    fallback for node/gateway (that would hand them Controller authority).
  - **First-node Node-seed sequencing (committed, one path):**
    1. Keeper install renders node/gateway envs pointing at
       `/var/lib/ployz/nats/node.seed` and starts the units.
    2. `ployzd node` / `ployzd gateway` treat a missing/unreadable seed file
       as a **typed bounded-retry startup state**
       (`NodeCredentialState::AwaitingSeedFile`), re-reading the path on each
       bounded backoff tick, with visible health (the process health endpoint
       reports `awaiting-credentials`, not a crash loop). Wiring lands in B4;
       the env contract is fixed here.
    3. `ployzctl init activate-first-node` mints the first node's
       `Node{node_id}` NKey (B4 minting path). **The named writer of
       `node.seed` is ployzd control**, which runs on the same machine: after
       mint+render+reload+verify, control writes
       `/var/lib/ployz/nats/node.seed` (`0600`) directly — local file write,
       no RPC hop needed for first node.
    4. Pickup is **in-process re-read**: the awaiting node/gateway processes
       find the file on their next retry tick and connect. No systemd restart
       is required or issued.
  - **File-ownership table (extends the ADR-0014 split):**
    | Path | Writer | When |
    |---|---|---|
    | `/etc/nats/nats-server.conf` | keeper | install (once) |
    | `/etc/nats/authorized-users.conf` | keeper (initial), then ployzd control exclusively | install; every mint/remove |
    | `/var/lib/ployz/nats/{ca.pem,server.crt,server.key}` | keeper | install |
    | `/var/lib/ployz/nats/{controller.seed,operator.seed,join.seed}` | keeper | install |
    | `/var/lib/ployz/nats/node.seed` | ployzd control (first node) / keeper join commit (joined nodes, as `nats.creds`) | activate-first-node / join |
  - `crates/ployz-keeper/src/first_node_install_cli.rs` +
    `crates/ployzctl/src/commands/init.rs`: surface `--node-public-ip` into
    cert SANs and listener; init output prints the operator seed + CA paths
    for ployzctl use.
  - `crates/ployz-keeper/src/local.rs`: join path stores the redeemed
    per-machine seed as the existing `nats.creds` file (it finally gets read).
- **Tests rewritten:** `crates/ployz-keeper/tests/bootstrap.rs`,
  `tests/local.rs`, `tests/systemd.rs` — env-file expectations now contain
  `PLOYZ_NATS_URL=tls://`, `PLOYZ_NATS_CA_FILE`, `PLOYZ_NATS_NKEY_SEED_FILE`
  (node/gateway pointing at `node.seed`); rendered nats config contains
  `tls {` and `include`; new unit tests for `nats_identity.rs` (SAN coverage,
  seed prefixes, PEM round-trip).
- **Verification:** `cargo test -p ployz-keeper`.

### B4. ployzd wiring + per-machine minting at machine-add

- **Goal:** all ployzd roles and ployzctl connect authenticated; `machine add`
  returns its operation id quickly and mints real per-machine credentials as
  bounded operation work with typed failure evidence, fenced single-writer
  renders, and an ADR-0001-classified authority file.
- **Rewrite:**
  - `crates/ployzd/src/config.rs`: `load_nats_connect_config()` reads
    `PLOYZ_NATS_URL` + new `PLOYZ_NATS_CA_FILE` + `PLOYZ_NATS_NKEY_SEED_FILE`
    into `NatsConnectConfig`; typed error variants for each missing/invalid
    input. `DaemonProcessConfig` role variants carry `NatsConnectConfig`
    instead of bare `NatsClientUrl`. A present-but-missing seed **file** (path
    configured, file absent) is not a config error for node/gateway roles —
    it enters the `AwaitingSeedFile` bounded-retry state from B3.
  - `crates/ployzd/src/control_runtime.rs:43`,
    `node_process_runtime.rs:52` (+ the second bare connect at 731),
    `gateway_process_runtime.rs:74` (today a bare `connect_with_timeout`,
    import at line 14): switch to `connect_authenticated`. Gateway/DNS use the
    machine's Node `NatsConnectConfig` (no Gateway principal — see B1).
    Node/gateway implement the `AwaitingSeedFile` startup state (typed enum,
    bounded backoff, visible health) committed in B3.
  - `crates/ployzctl/src/runtime.rs:658`: same; ployzctl reads the same three
    envs (operator seed, `NatsPrincipal::User`).
  - **Create** `crates/ployzd/src/nats_authorization.rs`: owns
    `/etc/nats/authorized-users.conf`.
    - Truth model (ADR-0001 classification): the authorized principal set in
      `KV_CORE` (`NatsAuthorizedUser` records keyed by principal) is
      explicitly named durable authority; the on-disk file is its recovery
      evidence and survives JetStream loss. **On control start and before any
      render, read the existing file and adopt unknown entries into KV as
      observations.** A render that would shrink the user set relative to the
      file is refused unless it is a step of an explicit machine-remove
      operation — renders never silently revoke credentials.
    - Fencing (ADR-0015): all read-set→render→reload→verify work serializes
      through a **single-writer owned task** inside `nats_authorization.rs`
      (mpsc of render requests, one consumer). Concurrent machine-adds queue;
      no two renders interleave on the single file resource.
    - Reload runs through a small `NatsReloadRunner` trait (real impl:
      `systemctl reload nats-server`; test impl records calls — a hard test
      seam, allowed by AGENTS).
  - **Minting locus (small handlers, fast operation ids):** the machine-add
    handler path (`crates/ployzd/src/operation_api.rs:615` →
    `machine_add_bootstrap_material` at 727–744 →
    `controllers.rs` `issue_machine_add_bootstrap_material` at 299–330)
    today builds material synchronously inside the RPC handler. That changes:
    - The handler keeps the join-token skeleton (fingerprint, TTL,
      single-use) and returns **operation id + join token + join bundle
      only** — non-secret material available at submit time (the install line
      needs only token + URL + CA + the cluster-static Join seed). The
      handler does not mint, render, reload, or test-connect.
    - Minting runs as **bounded operation work after acceptance** (the
      operation owner/worker side): generate node NKey keypair (dep: `nkeys`
      in ployzd), upsert `Node{node_id}` into the KV principal set, submit a
      render request to the `nats_authorization` single-writer, await
      render+reload, bounded test-connect with the minted seed, then store the
      per-machine `MachineJoinSecretDelivery { nats_credentials: <seed> }` in
      the status store. Operation events mark each step: `minted`,
      `rendered`, `reloaded`, `verified`, `material-ready`.
    - New typed terminal failure variants on the machine-add operation:
      `AuthorizationRenderFailed`, `NatsReloadFailed`,
      `MintedCredentialUnusable` — with evidence (command output), retained
      per operation rules.
    - **Idempotency replay stays consistent** (`operation_api.rs:728–744`): a
      retried submit returns the already-issued token/bundle and the
      already-minted material if present — it never mints twice. The
      `submitted_machine_add_bootstrap_material` / status-store
      secret-delivery records (`ployz-nats/src/operations/status_store.rs:54,
      363–434`) reshape accordingly: the stored bootstrap material no longer
      embeds `secret_delivery`; the minted secret is a separate record written
      by the worker (`put_machine_add_secret_delivery_if_absent` already
      models write-once).
    - Keeper redeem waits: redeem before `material-ready` gets a typed
      "not ready" response; the keeper retries boundedly until material-ready
      or token TTL expiry.
  - **Template field drop lands here (moved from B1):**
    `MachineJoinTemplate` drops `secret_delivery` — the template file is
    non-secret material only. Same-commit sweep of its consumers:
    `crates/ployzd/src/controllers.rs` (fields/uses at 52, 69, 197, 232–244,
    328, 532), `crates/ployz-nats/src/operations/status_store.rs`
    (`StoredMachineAddSecretDelivery` at 54 + the record fns at 363–434),
    `crates/ployzctl/src/commands/init/join_template.rs` (**remove the
    `--secret-delivery-file` flag**, `secret_delivery_file` field at 156/188,
    `read_secret_delivery` at 298–306) and the usage text in
    `crates/ployzctl/src/commands.rs:18`, plus the
    `crates/ployzd/tests/role_process.rs` fixture's `secret_delivery` block
    (line 759, already reshaped once in A2 — drops entirely here),
    `crates/ployzd/src/config.rs` join-template loading
    (`PLOYZ_MACHINE_JOIN_TEMPLATE_FILE`): template carries bundle material
    only, `trusted_nats.ca_pem`.
  - `activate-first-node` mints the first node's `Node{node_id}` user through
    the same worker-side path, then ployzd control writes
    `/var/lib/ployz/nats/node.seed` locally (B3 sequencing).
  - `crates/ployzctl/src/commands/machine.rs`: install line gains
    `PLOYZ_NATS_URL=tls://<core>:4222`, `PLOYZ_NATS_CA_B64=<base64 ca.pem>`,
    `PLOYZ_JOIN_NKEY_SEED=<join seed>` env prefixes consumed by
    `scripts/ployz.sh`; `scripts/ployz.sh` writes the CA to disk and passes
    both to keeper.
  - `crates/ployz-keeper/src/main.rs` join redeem (163, 221): connect with
    Join seed + CA over TLS (Join inbox prefix); bounded redeem retry until
    material-ready/TTL; store the redeemed per-machine seed at `nats.creds`;
    joined-node role envs point `PLOYZ_NATS_NKEY_SEED_FILE` at it (node and
    gateway both — Node principal).
- **Test-suite flips in this step** (rule: **every test that spawns
  `nats-server` or runs product binaries/runtimes against `PLOYZ_NATS_URL`
  flips to `SecuredTestNats` when its product code path starts requiring
  auth**; find them with `grep -rln 'nats_server::run_server' crates/` and
  `grep -rln PLOYZ_NATS_URL crates/`):
  - ployzd integration suites: `control_runtime.rs`, `node_runtime.rs`,
    `node_rpc.rs`, `node_service_runtime.rs`, `deploy_runtime_nats.rs`,
    `deploy_command_preparation_nats.rs`, `dns_source_nats.rs`,
    `gateway_process_runtime.rs`, `gateway_source_nats.rs`,
    `backup_restore.rs`, `nats_bootstrap.rs` (now spawns against rendered
    TLS configs with fixture certs), `role_process.rs`,
    `wireguard_dataplane.rs`.
  - the in-file `#[cfg(test)] TestNats` in
    `crates/ployzd/src/node_process_runtime.rs` (~718–740, used at 427, 591,
    622, 649) — replaced by the shared secured fixture.
  - ployzctl binary suites: `machine_add_binary_nats.rs`,
    `api_client_nats.rs`, `init_binary_nats.rs`, `ops_watch_binary_nats.rs`,
    `deploy_binary_nats.rs`.
- **Tests:** `crates/ployzd/tests/control_runtime.rs` (secured fixture;
  machine-add returns its operation id before any reload occurs; mints unique
  creds per add — two adds produce different seeds; reload runner invoked;
  reload-failure produces the typed terminal failure with evidence;
  **concurrency: two concurrent machine-adds both complete and both public
  keys are present in the rendered `authorized-users.conf`** (the ADR-0015
  fence test); **durability: with a pre-existing file containing an unknown
  user and an empty KV set, startup adopts the entry and a subsequent render
  does not shrink the file**; placeholder string `user-jwt-and-seed`
  eradicated), `crates/ployzctl/tests/machine_add_binary_nats.rs` +
  `api_client_nats.rs` (new output contract),
  `crates/ployz-nats/tests/operations_nats/*` (redeem returns minted secret
  only after material-ready; redeem-before-ready gets the typed not-ready
  response; secret deleted after report — existing behavior, new material).
- **Verification:** `cargo test -p ployzd -p ployzctl -p ployz-nats` — green
  **with the full suite list above flipped**, not just the two ployzctl
  suites; `grep -rn "user-jwt-and-seed" crates/ scripts/` empty.

### B5. Enforcement reconciliation + proof script de-hack

- **Goal:** every integration test runs against a TLS+authorization server;
  permission profiles match what the code actually does (denials surface here,
  not in production).
- **Rewrite:**
  - Flip the remaining plaintext suites to `SecuredTestNats` (everything not
    already flipped in B4 — the closing sweep; re-run
    `grep -rln 'nats_server::run_server' crates/` and assert only the fixture
    itself remains): `crates/ployz-nats/tests/bootstrap.rs`,
    `observations_nats.rs`, `core_state_nats.rs`, `operations_nats/*`
    (fixtures + submission), (delete
    `crates/ployz-nats/tests/configs/jetstream.conf` or keep solely for
    crate-local JetStream unit tests that don't exercise product auth),
    `crates/ployz-e2e/tests/operations.rs` + `tests/support/nats.rs`
    (support fixture now wraps `ployz_test_support::nats`).
  - Fix every permission denial by editing the profile in
    `crates/ployz-core/src/permissions.rs` (never by widening to `>` and
    never by widening an inbox allow beyond the principal's own prefix); each
    fix gets a regression assertion in
    `crates/ployz-nats/tests/secured_connect.rs`. Gateway/DNS denials are
    Node-profile fixes by design (no Gateway principal exists to widen).
  - `scripts/local-dataplane-proof.sh`: delete the
    `sed host: 0.0.0.0 + restart` hack (line 282) — init now takes
    `--node-public-ip` and renders the external TLS listener; the template
    heredoc drops `secret_delivery`; edge join uses the printed
    `PLOYZ_JOIN_NKEY_SEED`/`PLOYZ_NATS_CA_B64` envs.
- **Verification:** `cargo test --workspace` (all suites against secured
  servers); run `bash scripts/local-dataplane-proof.sh` once locally to
  confirm the two-machine bash flow still passes end-to-end with real auth.

---

## Phase C — Docker-in-Docker E2E Harness

Design source: uncloud's `ucind` (privileged DinD machine containers, one
labeled bridge network per cluster, cluster formed only through product
commands, label-based cleanup) merged with the proven systemd recipe from
`scripts/local-dataplane-proof.sh` Layer B (keeper requires
`LinuxRootSystemd`, so machines boot systemd as PID 1 rather than the
`docker:dind` image).

### C1. Machine image and artifact pipeline

- **Goal:** one local image that can be a ployz "machine": systemd PID 1,
  inner dockerd, glibc-compatible with host-built binaries, no registry pulls
  at test time.
- **Create:**
  - `docker/dind-machine/Dockerfile`: `FROM debian:bookworm` (glibc matches
    `rust:1.91-bookworm` builds) + `systemd systemd-sysv docker.io
    wireguard-tools iproute2 iptables ca-certificates curl`; `nats-server`
    v2.14.2 binary baked at `/usr/local/bin/nats-server`; workload image
    tarball baked at `/images/nginx.tar` (crane/`docker save` at build time)
    with a oneshot `ployz-dind-images.service` systemd unit that
    `docker load`s `/images/*.tar` after docker.service — inner daemons never
    pull. ployz binaries are **not** baked (volume-mounted, see below) so the
    image is rebuilt rarely (avoids uncloud's stale-image footgun for slow
    Rust builds).
  - `scripts/build-dind-machine-image.sh`: builds/locates linux-amd64 release
    binaries via the existing `scripts/prepare-h0-artifacts.sh` docker-build
    path, then `docker build -t ployz-dind-machine:local docker/dind-machine`.
- **Verification:** `bash scripts/build-dind-machine-image.sh` then
  `docker run --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw
  --tmpfs /run ployz-dind-machine:local /sbin/init` reaches
  `systemctl is-system-running` ∈ {running, degraded} and `docker info`
  succeeds inside.

### C2. Harness library in ployz-e2e

- **Goal:** a Rust provisioner replacing the bash orchestration: typed
  machines, per-run identifiers, readiness polling, evidence capture,
  label-based cleanup.
- **Create:** `crates/ployz-e2e/src/dind/` (the crate already has
  `src/lib.rs`): `mod.rs`, `cluster.rs`, `machine.rs`, `exec.rs`,
  `evidence.rs`. Dev-dependency `bollard` (same client ployzd uses; client
  from env so Docker Desktop works).
  - Types: `DindRunId` (nuid-based, in every resource name),
    `DindCluster { run_id, network_name, core: DindMachine,
    edges: Vec<DindMachine> }`,
    `DindMachine { name, container_id, bridge_ip, published: PublishedPorts }`,
    `PublishedPorts { nats: SocketAddr, gateway: SocketAddr }` (pre-reserved
    `127.0.0.1` ports bound explicitly — Docker re-randomizes published ports
    on restart, the uncloud lesson),
    `MachineSpec { role: DindMachineRole::{Core, Edge}, image }`.
  - Behavior: constant label `dev.ployz.dind.managed=true` +
    `dev.ployz.dind.run=<run_id>` on network and containers; idempotent
    pre-create sweep by label; create bridge network; run privileged
    containers (`/sbin/init`, `--cgroupns=host`, `/sys/fs/cgroup:rw`, tmpfs
    `/run` + `/run/lock`, stop signal `SIGRTMIN+3`,
    `RemoveVolumes: true` on teardown); mount host artifact dir ro at
    `/opt/ployz/artifacts` (file:// sources in the join template — accepted
    local shortcut); wait for `systemctl is-system-running` and inner
    `docker info` via exec with bounded backoff (90s budget); `exec.rs`
    returns `ExecOutcome { exit_code, stdout, stderr }` (no silent failures);
    `evidence.rs` dumps `journalctl -u nats-server -u 'ployzd-*'`,
    `systemctl --failed`, inner `docker ps -a`, and
    `/etc/nats/authorized-users.conf` into `target/dind-evidence/<run_id>/`
    on any assertion failure; cleanup is explicit `DindCluster::teardown()` +
    the label sweep script (no Drop-only reliance across panics).
  - Env gate: tests early-return unless `PLOYZ_DIND_E2E=1` (the
    `wireguard_dataplane.rs` pattern); `PLOYZ_DIND_KEEP=1` skips teardown for
    debugging.
- **Create:** `scripts/dind-clean.sh`: one-liner sweep
  (`docker ps -aq --filter label=dev.ployz.dind.managed | xargs docker rm -fv`
  + network rm by label).
- **Verification:** `PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test
  dind_cluster boots_machine_image` (a smoke test that provisions one machine,
  asserts readiness, tears down, and leaves nothing labeled behind).

### C3. Cluster formation scenarios (init/activate, machine add)

- **Goal:** prove the real product path forms a TLS-authenticated cluster.
- **Create:** `crates/ployz-e2e/tests/dind_cluster.rs` (tokio tests, gated):
  - **Scenario 1 — init + activate-first-node:** exec inside core:
    `ployzctl init --run-keeper-install --node core_1 --gateway
    --node-public-ip <bridge_ip> --ployzd-source file:///opt/ployz/artifacts/...`
    then `ployzctl init activate-first-node`. Harness copies
    `/var/lib/ployz/nats/{ca.pem,operator.seed}` out of the core container
    and connects a **host-side** `ployzctl::api_client::OperationApiClient`
    through the published `127.0.0.1` NATS port (works because B3 puts
    `127.0.0.1` in the server-cert SANs). Asserts: activate operation reaches
    Completed with the expected event sequence (including the mint events
    `minted`/`rendered`/`reloaded`/`verified`); `nats-server.service`,
    `ployzd-control.service`, `ployzd-node-core_1.service`,
    `ployzd-gateway.service` active; **`/var/lib/ployz/nats/node.seed` exists
    after activate and the node/gateway units left the awaiting-credentials
    state without a restart** (B3 sequencing); the core node's public key is
    in `/etc/nats/authorized-users.conf`; bootstrap KV/streams exist.
  - **Scenario 2 — machine add via join bundle:** host-side `machine add`
    via the API client; the submit response carries the operation id
    immediately (assert response latency does not include the reload: the
    `reloaded` event lands after acceptance); parse join token + install env
    from the operation result; exec `scripts/ployz.sh` flow on the edge
    container with `PLOYZ_NATS_URL=tls://<core_bridge_ip>:4222`,
    `PLOYZ_NATS_CA_B64`, `PLOYZ_JOIN_NKEY_SEED`. Asserts: join operation
    Completed; machine state active with `nats_connection` readiness
    evidence; edge
    `/var/lib/ployz/keeper/join-material.d/nats.creds` exists and differs
    from the core controller seed; core
    `/etc/nats/authorized-users.conf` contains the edge node's public key
    **alongside the previously present users (never-shrink)**; the edge
    gateway/DNS units authenticate with the same Node seed (no separate
    gateway credential exists — assert `PLOYZ_NATS_NKEY_SEED_FILE` in the
    gateway env points at `nats.creds`); token re-redeem refused (single-use
    evidence preserved).
- **Verification:** `PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test
  dind_cluster -- --test-threads=1 scenario_init scenario_machine_add`.

### C4. Deploy, daemon-restart invisibility, auth rejection

- **Goal:** the three behavioral guarantees v1 must keep.
- **Extend:** `crates/ployz-e2e/tests/dind_cluster.rs`:
  - **Scenario 3 — cross-machine deploy:** deploy the baked nginx image with
    replicas placed on both machines and a route; assert deploy operation
    event sequence (operations.rs assertion vocabulary), inner
    `docker ps` on both machines shows the managed containers with
    `dev.ployz.*` labels, and HTTP responses arrive through **both** gateways
    via the published `127.0.0.1` gateway ports.
  - **Scenario 4 — daemon-restart invisibility:** with the deploy serving,
    exec `systemctl restart ployzd-control` on core and
    `systemctl restart ployzd-node-<id>` on the edge; assert: gateway HTTP
    keeps answering throughout (poll during restart window), workload
    container IDs unchanged after restart (adopt-not-recreate), and the
    operations API answers again after reconnect with no mutated machine
    state.
  - **Scenario 5 — auth rejection:** (a) host client with a freshly generated
    random NKey seed + correct CA → connection refused (authorization
    violation); (b) host client with **no TLS** to the published port →
    handshake failure; (c) client using the edge node's minted seed
    publishing to the core node's service scope
    (`plz.v1.svc.node.core_1.>`) and writing `$KV.KV_CORE.>` → permission
    violation; (d) client using the cluster's Join seed subscribing
    `_INBOX.>` and the core node's inbox prefix → permission violation
    (inbox isolation holds in the real cluster, not just the fixture) — and
    the cluster remains healthy after all of (a)–(d) (gateway still serves).
- **Verification:** `PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test
  dind_cluster -- --test-threads=1` (full suite).

### C5. Dev-loop wrapper and docs

- **Goal:** one command for humans and CI; documented cleanup.
- **Create:** `scripts/dind-e2e.sh`: builds artifacts + image if stale (hash
  of binaries vs a marker file), then runs the gated test suite with
  `PLOYZ_DIND_E2E=1`. `docs/operations/dind-e2e.md`: requirements
  (Docker with `--privileged`, ~4GB per machine pair), gating envs, evidence
  dir, `scripts/dind-clean.sh`.
- **Rewrite:** `scripts/local-dataplane-proof.sh`: add a header noting Layer B
  (two-machine cluster) is superseded by the DinD harness; Layer A (the
  WireGuard/eBPF `wireguard_dataplane.rs` proof) remains the script's job.
- **Verification:** `bash scripts/dind-e2e.sh` passes from a clean checkout
  (after `scripts/build-dind-machine-image.sh`); `bash scripts/dind-clean.sh`
  leaves `docker ps -a --filter label=dev.ployz.dind.managed` empty.

---

## Deleted vs Rewritten (roll-up)

**Deleted files:** `crates/ployzd/src/iroh_tunnel.rs`,
`crates/ployzd/tests/iroh_nats_tunnel.rs`,
`crates/ployz-transport/src/iroh_endpoint.rs`,
`crates/ployz-transport/src/nats_tunnel.rs`,
`crates/ployz-transport/tests/nats_tunnel.rs`,
`crates/ployz-e2e/tests/h0_script.rs`,
(`crates/ployz-nats/tests/configs/jetstream.conf` if fully superseded in B5).

**Deleted symbols/fields:** `DaemonProcessRole::Tunnel`, `TunnelSide`,
`TunnelProcessConfig`, `TunnelWork`, all `PLOYZ_TUNNEL_*` envs,
`ployzd tunnel`/`tunnel identity` CLI, `ployzd-tunnel-{core,edge}.service`,
`MachineJoinCoreIrohEndpoint`, `MachineJoinIroh{PublicKey,DirectAddress,RelayUrl,Ticket}`,
`MachineJoinEdgeTunnel`, `core_iroh`/`core_iroh_ticket` JSON fields,
`JOIN_CORE_IROH_TICKET_FILE`, `EdgeNatsTunnel` test helper,
`render_bootstrap_tunnel_command`, `--core-iroh-*` flags,
`MachineJoinTrustedNats.config_sha256`, `MachineJoinTemplate.secret_delivery`
(B4) + the `--secret-delivery-file` ployzctl flag, the shared
`RESPONSE_INBOX = "_INBOX.>"` permission constant, the in-file `TestNats` in
`node_process_runtime.rs`, placeholder credential `"user-jwt-and-seed"`.

**Rewritten (kept, new shape):** `ployz-core` `install.rs`/`roles.rs`/
`machine.rs`/`nats_config.rs`/`security.rs` (+ new `permissions.rs`, updated
`tests/nats_config.rs`), `ployz-nats` `connect.rs`/`operations/status_store.rs`,
`ployzd` `config.rs`/`app.rs`/`daemon_runtime.rs`/`main.rs`/`controllers.rs`/
`operation_api.rs`/`nats_process.rs`/`node_process_runtime.rs`/
`gateway_process_runtime.rs` (+ new `nats_authorization.rs`), `ployzd`
`tests/nats_bootstrap.rs`/`tests/role_process.rs` (+ the full B4 suite list),
`ployz-keeper` `steps.rs`/`join.rs`/`local.rs`/`systemd.rs`/`main.rs` (+ new
`nats_identity.rs`), `ployzctl` `commands/machine.rs`/`init/join_template.rs`/
`runtime.rs`, `ployz-e2e` `operations.rs`/`support/nats.rs` (+ new `src/dind/`),
`ployz-sdk-types` exports + snapshots, `ployz-test-support` (+ new `nats.rs`),
`scripts/local-dataplane-proof.sh`, `scripts/ployz.sh`.

## Risks Carried Into Execution

- A2 is the schema cliff: `deny_unknown_fields` means every embedded
  join-template JSON fixture must land in the same commit as `install.rs`.
  The enumerated fixture set (8+ files + the proof script heredoc):
  `ployzd/tests/control_runtime.rs`, **`ployzd/tests/role_process.rs`
  (`temp_join_template_file`)**, `ployzd/tests/backup_restore.rs`,
  `ployz-nats/tests/operations_nats/{fixtures,submission}.rs`,
  `ployzctl/tests/{init_join_template_cli_contract,machine_add_binary_nats,api_client_nats,cli_contract}.rs`,
  `ployz-keeper/src/main.rs` + `tests/local.rs`,
  `scripts/local-dataplane-proof.sh`.
- B1 is a second, smaller cliff: `NatsServerConfig` gaining required
  `tls`/`listener`/`include` breaks `ployzd/src/nats_process.rs`,
  `ployzd/tests/nats_bootstrap.rs`, `ployz-core/tests/nats_config.rs`, and
  `ployz-keeper/src/steps.rs:757`; `MachineJoinTrustedNats` reshape breaks
  `ployz-keeper/src/join.rs:85` + `steps.rs` `RedactedJoinMaterial` — all
  swept in the B1 commit. `MachineJoinTemplate.secret_delivery` is
  deliberately **not** dropped until B4, where minting replaces it.
- The working tree on `feat/step-1` is dirty across many of these files —
  rebase/sequence against in-flight work before starting A1.
- async-nats 0.49 NKey feature flag must be verified (`nkeys` is not in
  Cargo.lock with current features); if `with_nkey` is unavailable under the
  pinned feature set, fall back to `.creds`-file auth
  (`with_credentials_file`) — the join-bundle field is opaque either way.
  `custom_inbox_prefix` is core API in 0.49 (verified in the vendored
  source), no feature needed.
- Permission-vs-reality denials in B5 are expected; budget the reconciliation
  pass, fix profiles narrowly (and never widen inbox allows beyond the
  principal's own prefix).
- Worker-side minting introduces a redeem-before-ready window; the keeper's
  bounded redeem retry and the typed not-ready response must be tested
  against token TTL expiry.
- DinD requires `--privileged`; CI runner must allow it. Parallel clusters are
  memory/disk heavy — first version serializes (`--test-threads=1`).

## Verification

Run at every phase boundary; all must be green before the next phase:

```sh
# workspace health
cargo test --workspace
cargo clippy --workspace --all-targets

# iroh is gone (after A3)
cargo tree -p ployzd | grep -i iroh && echo FAIL || echo OK
grep -rn "core_iroh\|PLOYZ_TUNNEL_\|TunnelSide" crates/ && echo FAIL || echo OK

# real auth (after B5)
cargo test -p ployz-nats --test secured_connect
cargo test -p ployzd --test control_runtime
grep -rn "user-jwt-and-seed" crates/ scripts/ && echo FAIL || echo OK
grep -rn '"_INBOX\.>"' crates/ && echo FAIL || echo OK   # no shared-inbox grants
bash scripts/local-dataplane-proof.sh   # two-machine bash flow, real TLS+creds

# DinD e2e (after C4/C5)
bash scripts/build-dind-machine-image.sh
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test dind_cluster -- --test-threads=1
bash scripts/dind-clean.sh
```