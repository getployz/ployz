# Cluster ingress and private access plan

A Cluster gets two ways for visitors to reach its apps, public ports or a BYO Cloudflare Tunnel, and one way for its team to reach every port privately, a reserved Tailscale subnet router.
The Cluster is all in on one door. Both doors exist together only while a switch runs.
The rule the program enforces is that the daemon builds every reserved Global from its own pinned images, and nothing outside the Machine can hand it a different one.
New Clusters get a setup card with good defaults. A user can still skip it and build a project first.
The design this plan builds is `docs/agents/design/cluster-ingress/design.md`.
Engine PRs land before Cloud PRs, in this order. E0, E1, E2, E3, E4, C1, C2, C3, C4, C5.

## How to read this

One box is one unit of work. Every box names the evidence that checks it. A nested box is a sub-step of the box above it. Check a box only when its evidence exists, a file, a log line, a screenshot, a test run, or a SHA. The body is a how-to. The appendices explain and record.

`<plugin root>` is the installed t3-pstack plugin's directory, the one that holds its `skills/` and `agents/`. Paths without a leading slash are relative to the repo root.

The program runs `<plugin root>/skills/poteto-mode/playbooks/autopilot-stack.md`. Owners stop every PR at verified and green. The operator lands every PR. No owner merges.

Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

## Program checklist

### Arm the program

- [ ] State the protocol and this plan to the operator, then stop. Start execution only on the operator's explicit go.
- [ ] Read these at program start. Re-read them at every tick.
  - [ ] `cat <plugin root>/skills/poteto-mode/playbooks/autopilot-stack.md`
  - [ ] `cat <plugin root>/skills/poteto-mode/playbooks/autopilot-full.md`
  - [ ] `cat <plugin root>/skills/swarm/SKILL.md`
  - [ ] `cat .claude/skills/verify-server/SKILL.md`
  - [ ] `cat .claude/skills/verify-dashboard/SKILL.md`
  - [ ] `cat .claude/skills/verify-caddy-load/SKILL.md`
  - [ ] `cat .claude/skills/ui-copy/SKILL.md`
  - [ ] `cat <plugin root>/skills/poteto-mode/playbooks/opening-a-pr.md`
  - [ ] `cat <plugin root>/skills/interrogate/SKILL.md`
  - [ ] `cat <plugin root>/skills/show-me-your-work/SKILL.md`
  - [ ] `cat docs/agents/landing.md`
- [ ] On the operator's go, arm the audit tick as an hourly schedule_task bound to this thread with the tick prompt below. Never leave the cadence to memory.
- [ ] Use this tick prompt, verbatim. "Re-read the execution playbook from disk. Audit the operation against it and fix drift in this tick. Probe every active lane and judge progress by side effects only. Stand down a stuck lane and dispatch its replacement now. Then post a short status message to the operator in chat only when the audit found a tracked change that no earlier status message reported, such as a PR opened, a code-ready head, a round launched or closed, a verdict, a merge, a stuck agent and the action taken, a blocker added or cleared, or a decision only the operator can make. Name every such change and nothing else. Do not repeat a table, the merged list, or an unchanged blocker. If the audit found none, end the turn with no reply text. Either way, log this tick's row in your decision trail. The row names the items reported, or none."
- [ ] On the operator's hold or stand-down, send every owner a zero-writes order at once.

### Spawn owners

- [ ] Spawn one owner per PR with the full lifecycle the execution playbook names.
- [ ] Follow this dependency graph. Stack a child on its parent branch while the parent is open.
  - [ ] E0 and E3 are independent and first. Both branch from `main`.
  - [ ] E1 after E0.
  - [ ] E2 after E1.
  - [ ] E4 after E2, because it reuses E2's platform sealing context.
  - [ ] C1 after E3.
  - [ ] C2 after E2 and C1.
  - [ ] C3 after C2.
  - [ ] C4 after E4 and C1.
  - [ ] C5 after C2 and C4.
- [ ] Hold the file boundaries. E0 touches only `core/scripts/verify-cluster`, `core/scripts/verify/**`, and `.agents/skills/verify-server/SKILL.md` (`.claude/skills/verify-server` is a symlink to its directory). E1 to E4 touch only `core/**`, `docs/user/**`, `core/CONTEXT.md`, and `core/DESIGN.md`. C1 to C5 touch only `dashboard/**`, `docs/user/**`, and `dashboard/CONTEXT.md`.
- [ ] Hold the review gate. E2, E3, E4, C1, C2, C3, C4, and C5 change an interaction. They wait for the operator's review in chat with screenshots and a video before the operator lands them.
- [ ] Each PR body names every `core/DESIGN.md` bet it touches and the choice it changed, per `AGENTS.md`. Each engine PR body argues the Stable promise for its new route and capability.

### PR mechanics, for every PR

- [ ] Resolve the forge once. Default to `gh`; if `command -v origin` succeeds and Origin can resolve the repository, use `origin pr` for every PR operation. Record any fallback to `gh`. Never require `gt`.
- [ ] Open the PR ready, never draft, per **Opening a PR**. Use the run's built-in PR tool when it has one, else `origin pr create --status open --base <base-branch>` or `gh pr create --base <base-branch>` according to the resolved forge. A stack child targets its parent branch.
- [ ] Run the repo's lint and typecheck once before the PR-facing push. Push with hooks on. Engine PRs run `cargo fmt --check` and `cargo clippy -p <crate>` for the crates they touch. Cloud PRs run `pnpm typecheck` and `pnpm lint` in `dashboard/`. Never run the full suite locally.
- [ ] Run `/deslop` before each commit and `/no-comments` before review.
- [ ] Every PR, E0 to E4 and C1 to C5, runs the `ui-copy` skill at `.claude/skills/ui-copy/SKILL.md` before review, over every user-facing string it adds or changes. That covers CLI output and help, `ployz status` lines, refusal and error text, Approval cards and their terminal prompts, published-command descriptions that agents read, and `docs/user`. The PR body lists the strings it rewrote.
- [ ] Run `/interrogate` with the `interrogate reviewers` panel before opening each PR, per poteto-mode.
- [ ] Triage every Bugbot and security-reviewer comment per `../references/bugbot-triage.md`.
- [ ] Rebase onto current trunk before the code-ready report and babysit. Keep that merge base in fix rounds. Rebase again only at merge prep, on a `git merge-tree` conflict with trunk, on a CI failure that comes from a change on trunk, or when a known concurrent change to the same files lands.
- [ ] Never write to ployz.dev. Cloudflare lanes use the zone `zenploy.dev` with the `cf` CLI, `CLOUDFLARE_ACCOUNT_ID` set and `--force` on deletes. Every lane deletes the tunnels and records it created and lists the zone afterwards to prove it.
- [ ] Request every Tailscale auth key through `request_secret`. Prefer an ephemeral key. Never stage a key as a plain env value. Ask the operator to revoke the key when the lane ends.

### Verdict and merge, for every PR

- [ ] A full swarm runs once, at the code-ready head. Another full swarm runs only for new behavior, a redesign, or a large new diff. A rebase, a merge-conflict resolution, or a minor fix gets a single verifier at the new head, and a wording-only change gets none. Run each full swarm per `<plugin root>/skills/swarm/SKILL.md`. One gates lane. The ten live lanes from the PR's **Verify, live** block. The perf lane from its **Verify, perf** block. Two or more audit lanes, each with its own focus, that read the diff and the receipts and distrust the PR body. The root audits the receipts in the merge-ready report before the verdict.
- [ ] Judge every finding by the execution playbook's blocking rule. The verdict is clean when none blocks. A failure on trunk behavior in a lane marked `Regression lane against trunk.` is recorded against trunk and does not block the PR. A new head gets a fresh verdict, except for results that stay valid under the patch-id rule in `playbooks/shipping.md`.
- [ ] Stop at verified. Hand each PR to the operator with its verdict and head SHA. After the handoff nobody pushes without the operator's go. A restack that leaves a PR's `git patch-id` unchanged keeps its verdict.

### Boot recipe, for every live lane

Each live lane runs in its own worktree at the PR head. Engine lanes drive real Machines through verify-server. Cloud lanes drive the dashboard through verify-dashboard.

- [ ] `git fetch origin <head-branch> && git checkout <head SHA>`.
- [ ] Engine lanes. Run `run=$(core/scripts/verify-cluster up --machines 2)`, then `core/scripts/verify-cluster doctor "$run"`. Pre-pull the pinned pause, cloudflared, and tailscale digests with `exec` before any timed step.
- [ ] Cloud lanes. Run `SERVERS=1 scripts/verify/up.sh` in `dashboard/`, wait for its ready line, then drive the browser with agent-browser.
- [ ] Deliver input only through `core/scripts/verify-cluster cli`, `core/scripts/verify-cluster exec`, and agent-browser. Read-only diagnostics are `docker inspect`, `ss -ltn`, `journalctl -u ployz`, `dig`, and `ployz status --json`.
- [ ] Tear down with `core/scripts/verify-cluster down "$run"` or `scripts/verify/down.sh` in `dashboard/`. A Boat sandbox stops after one hour, so start lanes that wait on operator route approvals last.
- [ ] Save every screenshot to `/tmp/swarm-<pr-id>/worker-<n>/<slug>.png` and every terminal transcript to `/tmp/swarm-<pr-id>/worker-<n>/<slug>.txt`, and return the paths with the report.

## Add a joining Machine to a verify run (E0)

**Depends on.** None.

**Files.**

- [ ] Edit `core/scripts/verify-cluster`.
- [ ] Edit `.claude/skills/verify-server/SKILL.md`.

**Build.**

- [ ] Add the `add` action to `core/scripts/verify-cluster`. It boots one guest, enrolls it into the run's existing Cluster the way `up` enrolled the others, and prints the new Machine's name. Every later PR needs it to prove Global catch-up on a joining Machine.
- [ ] Document `add` in the verify-server skill's action list.

**You see.**

- [ ] `core/scripts/verify-cluster add "$run"` prints `machine-3`, and `core/scripts/verify-cluster cli "$run" -- server ls` lists three Machines.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `shellcheck core/scripts/verify-cluster` reports nothing new. Run `shellcheck core/scripts/verify-cluster`.
- [ ] `core/scripts/verify-cluster add --help` prints usage and exits 0. Run `core/scripts/verify-cluster add --help`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Run `up --machines 2` then `doctor` at trunk and head. Save `up-two.txt`. Pass when both runs report two healthy Machines.
- [ ] Lane 2. Run `add` on a two-Machine run. Save `add-third.txt`. Pass when `server ls` shows three Machines in state ready.
- [ ] Lane 3. Run `add --roles ingress` on a run whose Machines accept ingress, so the Ingress Proxy is the Global to catch up. A user Service cannot be made Global today. Save `global-catch-up.txt`. Pass when the new Machine runs the Ingress Proxy within five minutes.
- [ ] Lane 4. Run `add` twice. Save `add-twice.txt`. Pass when the run holds four distinct Machines and no name repeats.
- [ ] Lane 5. Run `add` on a run made with `--cloud`. Save `add-cloud.txt`. Pass when the Machine joins through the Cloud path and appears in the dashboard Servers list.
- [ ] Lane 6. Run `add` with a revoked join token. Save `add-revoked.txt`. Pass when it exits nonzero and names the token as the cause.
- [ ] Lane 7. Run `add`, then `down`. Save `down-after-add.txt`. Pass when `down` removes all three guests and no Boat sandbox stays listed.
- [ ] Lane 8. Run `add` with `--roles ingress`. Save `add-ingress-only.txt`. Pass when the new Machine reports only the ingress role.
- [ ] Lane 9. Run `doctor` after `add`. Save `doctor-three.txt`. Pass when doctor checks the third Machine too.
- [ ] Lane 10. Run `add` on a run whose first Machine is powered off. Save `add-peer-down.txt`. Pass when the new Machine still joins through the second Machine.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Wall time of `up --machines 2` at trunk and head, and wall time of `add` at head, which trunk lacks.
- [ ] Probe. Three interleaved runs of `up --machines 2` at trunk and head, then three timed `add` runs at head.
- [ ] Baseline. Record the trunk `up --machines 2` median first.
- [ ] Rule. Head `up` median within 10% of trunk. `add` median at or under six minutes. Either miss fails.

**Review gate.** None. E0 is not review-gated.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root hands E0 to the operator at verified. The operator lands it.

## Move the Ingress Proxy into a holder network namespace with pinned images (E1)

**Depends on.** E0.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/domain/ingress.rs`.
- [ ] Edit `core/crates/ployz-core/src/domain/spec.rs`.
- [ ] Edit `core/crates/ployz-core/src/value.rs`.
- [ ] Edit `core/crates/ployz-core/src/rpc_catalog.rs`.
- [ ] Edit `core/crates/ployz-core/src/rpc.rs`.
- [ ] Edit `core/crates/ployz-core/tests/capabilities.rs`.
- [ ] Edit `core/crates/ployzd/src/docker/create.rs`.
- [ ] Edit `core/crates/ployzd/src/docker/observe.rs`.
- [ ] Edit `core/crates/ployzd/src/docker/lifecycle.rs`.
- [ ] Edit `core/crates/ployzd/src/ingress.rs`.
- [ ] Edit `core/crates/ployzd/src/ingress/caddy.rs`.
- [ ] Edit `core/crates/ployzd/src/ingress/caddy/trace.rs`.
- [ ] Edit `core/crates/ployzd/src/machine/ingress.rs`.
- [ ] Edit `core/crates/ployzd/src/machine_api/local.rs`.
- [ ] Edit `core/crates/ployz/src/ingress.rs`.
- [ ] Edit `core/crates/ployz/src/global_catch_up.rs`.
- [ ] Edit `core/crates/ployz/src/drain/retirement.rs`.
- [ ] Edit the `MachineApi` test doubles in `core/crates/ployz/tests/cloud_init/harness.rs`, `core/crates/ployz/tests/deploy_client/support.rs`, `core/crates/ployz/tests/connect/support.rs`, and `core/crates/ployz-testkit/src/`.
- [ ] Delete `core/crates/ployz/src/ingress/caddy.rs` and the `--ingress-image` flag.
- [ ] Create `core/crates/ployzd/src/images.rs`.
- [ ] Edit `core/CONTEXT.md`, `core/DESIGN.md`, and `docs/user/servers/add-a-server.md`.

**Build.**

- [ ] Create `images.rs` with one constant per reserved image, each pinned by digest. Caddy and pause land here. E2 and E4 add cloudflared and tailscale.
- [ ] Add the reserved Service `ployz-system/ingress-holder` through `QualifiedService::system_ingress_holder()`. It runs the pinned pause image, owns the published ports, and is the row whose ports say whether the Public door is open. It is a Service Container, not a new `ContainerKind`, because that enum has no `#[serde(other)]` and a new variant breaks older readers.
- [ ] Add `IngressPorts { Public, Closed }` and `holder_service_spec(ports, constraints)`. `caddy_service_spec(constraints)` loses its image parameter and its ports. The validators return `IngressPorts` and compare images against the daemon's pins.
- [ ] Add a network-join field to `ServiceContainerSpec`, following `PidMode::Container`, with `#[serde(default)]`. `docker/create.rs` resolves it to `container:<holder id>` and skips `dns`, `dns_search`, `port_bindings`, and `extra_hosts` in that mode.
- [ ] Add the route `SetIngressDoors { doors }` with capability `ployz.ingress.doors.v1` and scope Operator. The daemon builds the holder and Caddy specs from its own pins and starts the holder before Caddy. E1 accepts only the Public door.
- [ ] Make the route the only way to place a reserved door container. `follow_roles`, `slot_repairs`, and Global catch-up call `SetIngressDoors` with the observed doors instead of `EnsureContainer` with a copied spec. This fixes pin skew during a rolling upgrade and the holder-before-Caddy order in one place.
- [ ] Keep older clients working. When an older CLI sends `EnsureContainer` with the old canonical Caddy spec to a Machine with the ingress role, the daemon treats it as `SetIngressDoors { Public }`. Any other spec for a reserved door identity is refused with a reason that names the CLI upgrade. A compat test shows that an older catch-up copying a new portless spec cannot place Caddy on a Machine without the ingress role.
- [ ] Extend stranded start in `docker/observe.rs` and `docker/lifecycle.rs` to the holder. Start the holder first, then Caddy.
- [ ] Render the loopback site `http://:8480` bound to `127.0.0.1` and `::1` in `render_caddyfile`. It proxies every hostname with or without certificate material. Scope `trusted_proxies 127.0.0.1 ::1` to a `servers :8480` block so loopback is not trusted on 80 or 443. Redirect to https only on `X-Forwarded-Proto: http`.
- [ ] Update `core/CONTEXT.md`. The Ingress Proxy is the holder plus Caddy. Add the term Door.
- [ ] Update bet 10 in `core/DESIGN.md`. The daemon starts the Ingress Proxy's two containers unasked, not one.
- [ ] Update the ports section of `docs/user/servers/add-a-server.md` to say the holder publishes 80 and 443.
- [ ] Name bets 3, 6, and 10 and the Stable promise in the PR body.

**You see.**

- [ ] `docker ps` on an ingress Machine shows `ployz-system-ingress-holder` with `0.0.0.0:80->80/tcp` and `0.0.0.0:443->443/tcp`, and the Caddy container shows no ports.
- [ ] `docker kill -s KILL <caddy>` leaves the holder running. Caddy restarts in place and `curl https://<app host>` answers 200 again.
- [ ] `ployz server add --ingress-image` fails with `unknown argument`.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `domain::ingress` gains round trips for both `IngressPorts` shapes and a refusal of a foreign image digest. Run `cargo test -p ployz-core --lib domain::ingress` in `core/`.
- [ ] `capabilities.rs` gains `ployz.ingress.doors.v1`. Run `cargo test -p ployz-core --test capabilities` in `core/`.
- [ ] `ingress::tests` gains a case that `follow_roles` calls `SetIngressDoors`. Run `cargo test -p ployz --lib ingress::tests` in `core/`.
- [ ] `global_catch_up` gains a case that catch-up of the ingress Global calls the route and never copies a spec. Run `cargo test -p ployz --lib global_catch_up` in `core/`.
- [ ] `docker::observe::tests` gains a stranded holder case that starts the holder before Caddy. Run `cargo test -p ployzd --lib docker::observe::tests` in `core/`.
- [ ] `docker::tests` gains the `container:` network mode body with no port bindings. Run `cargo test -p ployzd --lib docker::tests` in `core/`.
- [ ] `caddy_tests::trace` gains a case that `trusted_proxies 127.0.0.1` appears only under `servers :8480`. Run `cargo test -p ployzd --lib caddy_tests` in `core/`.
- [ ] `machine_api::local_tests` gains the old-canonical translation and the reserved refusal. Run `cargo test -p ployzd --lib machine_api::local_tests` in `core/`.
- [ ] The Caddy load model still passes. Run the `verify-caddy-load` skill's model check.
- [ ] The ingress cluster test passes. Run `PLOYZ_TESTKIT_IMAGE=$(scripts/testkit-image.sh ployz-testkit-checkout:dev) cargo test -p ployz --test ingress_cluster -- --ignored --test-threads=1` in `core/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Deploy `whoami` with a hostname, then curl 80 and 443 on both Machines at trunk and head. Save `public-door.txt`. Pass when every curl answers 200 on both sides.
- [ ] Lane 2. Kill Caddy with SIGKILL on one Machine. Save `caddy-crash.txt`. Pass when the holder's start time is unchanged, Caddy restarts, and 443 answers 200 within ten seconds.
- [ ] Lane 3. Run `docker restart <caddy>`. Save `caddy-restart.txt`. Pass when the holder keeps the ports and 443 answers 200 within ten seconds.
- [ ] Lane 4. Stop the daemon, stop the holder and Caddy, start the daemon. Save `stranded-start.txt`. Pass when the holder starts before Caddy in `journalctl -u ployz` and 443 answers 200.
- [ ] Lane 5. Power the Machine off and on with `power off` and `power on`. Save `boot.txt`. Pass when the holder and Caddy run and 443 answers 200 after boot.
- [ ] Lane 6. Run `server set --ingress off` then `--ingress on`. Save `ingress-toggle.txt`. Pass when the holder and Caddy go away and come back and 443 answers 200.
- [ ] Lane 7. Run `add`, giving a third Machine the ingress role. Save `catch-up.txt`. Pass when the joiner runs the holder and Caddy from its own pins, shown by `docker inspect` image digests matching `images.rs`.
- [ ] Lane 8. From a bridge container, send `CF-Connecting-IP` to 443 and try `127.0.0.1:8480` on the holder. Save `trust-scope.txt`. Pass when 443 logs the bridge peer as the client and 8480 is unreachable from the bridge.
- [ ] Lane 9. Inside the holder netns, curl `http://127.0.0.1:8480` with `Host` set to the app and `CF-Connecting-IP: 198.51.100.42`. Save `loopback.txt`. Pass when it answers 200 and the access log shows client `198.51.100.42`.
- [ ] Lane 10. Run a trunk CLI against the head daemon and run `server set --ingress on`. Save `old-cli.txt`. Pass when the daemon translates the old spec, the holder and Caddy run, and 443 answers 200.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. p50 and p99 latency of 500 requests to 443, and seconds from a Caddy SIGKILL to the first 200, at trunk and head.
- [ ] Probe. Two runs, one at trunk and one at head, interleaved in five rounds of 100 requests each with `hey`, then three kill-to-200 timings each.
- [ ] Baseline. Record the trunk p50, p99, and recovery median first.
- [ ] Rule. Head p50 and p99 within 10% of trunk, and head recovery median at or under trunk plus one second. Either miss fails.

**Review gate.** None. E1 is not review-gated.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto its parent branch after the verdict, patch-id unchanged.
- [ ] The root hands E1 to the operator at verified. The operator lands it after E0.

## Open the Tunnel door with a sealed token (E2)

**Depends on.** E1.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/sealed.rs`.
- [ ] Edit `core/crates/ployz-core/src/value.rs`.
- [ ] Create `core/crates/ployz-core/src/domain/tunnel.rs`.
- [ ] Edit `core/crates/ployz-core/src/rpc_catalog.rs` and `core/crates/ployz-core/src/rpc.rs`.
- [ ] Edit `core/crates/ployz-core/tests/capabilities.rs`.
- [ ] Edit `core/crates/ployzd/src/images.rs`.
- [ ] Edit `core/crates/ployzd/src/sealed_env.rs`.
- [ ] Edit `core/crates/ployzd/src/machine_api/local.rs`.
- [ ] Edit `core/crates/ployzd/src/certificates.rs`.
- [ ] Edit `core/crates/ployzd/src/ingress/caddy.rs`.
- [ ] Create `core/crates/ployz/src/handlers/ingress.rs`.
- [ ] Edit `core/crates/ployz/src/cli/mod.rs`.
- [ ] Edit `core/crates/ployz/src/handlers/server.rs`.
- [ ] Edit `core/crates/ployz/src/global_catch_up.rs`.
- [ ] Edit `core/crates/ployz/src/sdk.rs` and `core/crates/ployz-sdk/src/lib.rs`.
- [ ] Edit `core/crates/ployz/src/approval.rs`.
- [ ] Edit `core/CONTEXT.md`.
- [ ] Create `docs/user/services/cloudflare-tunnel.md`.
- [ ] Edit `docs/user/services/domains.md`, `docs/user/troubleshooting/app-not-reachable.md`, and `docs/user/servers/add-a-server.md`.

**Build.**

- [ ] Add a platform sealing purpose `ingress-tunnel` beside `SealedIdentity`, with its own info string and AAD bound to the Cluster. The daemon opens it next to `sealed_env.rs`.
- [ ] Add `QualifiedService::system_tunnel()` and `tunnel_service_spec()`. The spec runs the pinned cloudflared image with `network` joined to the holder, `--url http://127.0.0.1:8480`, `--token-file` on a tmpfs mount, and no published ports.
- [ ] Write the opened token into the tmpfs between create and start, reusing the step that delivers Build secrets.
- [ ] Extend `SetIngressDoors` with `sealed_token`, `#[serde(default)]`. Accept `Tunnel` only behind the new capability `ployz.ingress.tunnel.v1`, so a client checks the capability before it sends `Tunnel` and an E1 daemon never sees it.
- [ ] Render the holder `Closed` when the Public door is not in the set. Doors `{Public, Tunnel}` hold both open while a switch runs.
- [ ] Skip ACME on Closed Machines in `certificates.rs`. A Closed Machine leaves `machine_rank` and `ingress_challenge_ips`. With no open Public door anywhere, the run records a refusal reason like the policy path does.
- [ ] Global catch-up starts the tunnel on a joiner by calling `SetIngressDoors` with the replicated sealed token.
- [ ] Change `confirm_ingress_off` to count Machines with an open door, either Public or a running tunnel.
- [ ] Add `ployz ingress` to show each Machine's doors and `ployz ingress use public|tunnel [--token-stdin]` to switch. `--json` never prompts. Each Machine reports through `ui::finish_fanout`.
- [ ] Add the `PloyzSession` method `set_ingress_doors`, its napi binding, and the Rust approval `Verb` `setIngressDoors`.
- [ ] Add the terms Tunnel Connector and Platform sealing purpose to `core/CONTEXT.md`.
- [ ] Write `docs/user/services/cloudflare-tunnel.md`. Add the tunnel door to the ports table, to `domains.md`, and a tunnel section to `app-not-reachable.md`.
- [ ] Name bets 4, 6, 7, and 10 and the Stable promise in the PR body. Note that the route takes a Device signature later, per the np1 plan.

**You see.**

- [ ] `ployz ingress use tunnel --token-stdin` prints one row per Machine with `doors tunnel`, and `ss -ltn` on each Machine shows nothing on 80 or 443.
- [ ] `curl https://<app>.zenploy.dev` answers 200 through Cloudflare with no public port open.
- [ ] `ployz ingress --json` reports `"doors":["tunnel"]` for every ingress Machine.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `sealed` gains a round trip for the platform purpose and a refusal across Clusters. Run `cargo test -p ployz-core --lib sealed` in `core/`.
- [ ] `capabilities.rs` gains `ployz.ingress.tunnel.v1`. Run `cargo test -p ployz-core --test capabilities` in `core/`.
- [ ] `machine_api::local_tests` gains `Tunnel` with and without a token and the `Closed` holder shape. Run `cargo test -p ployzd --lib machine_api::local_tests` in `core/`.
- [ ] `certificates` gains a case that a Closed Machine is never ranked. Run `cargo test -p ployzd --lib certificates` in `core/`.
- [ ] `handlers::server` gains the open-door count for `confirm_ingress_off`. Run `cargo test -p ployz --lib handlers::server` in `core/`.
- [ ] `cli_shape` gains `ingress` and `ingress use`. Run `cargo test -p ployz --test cli_shape` in `core/`.
- [ ] `approval` keeps `every_operation_verb_cloud_sends_is_a_verb` green with `setIngressDoors`. Run `cargo test -p ployz --lib approval` in `core/`.
- [ ] The certificates cluster test passes. Run `PLOYZ_TESTKIT_IMAGE=$(scripts/testkit-image.sh ployz-testkit-checkout:dev) cargo test -p ployz --test certificates_cluster -- --ignored --test-threads=1` in `core/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Run the Public door scenario from E1 lane 1 at trunk and head. Trunk lacks the tunnel, so gate the head switch to the tunnel and the 200 through Cloudflare. Save `regression.txt`. Pass when both sides answer 200 on the Public door and head answers 200 through the tunnel.
- [ ] Lane 2. Create a tunnel on zenploy.dev, run `ingress use tunnel`. Save `use-tunnel.txt`. Pass when `ss -ltn` shows no 80 or 443 on either Machine and the app answers 200 through Cloudflare.
- [ ] Lane 3. Run `ingress use public` after lane 2. Save `use-public.txt`. Pass when 80 and 443 are published again and the tunnel container is gone.
- [ ] Lane 4. Loop curl every 200 ms during a switch from public to tunnel. Save `switch-no-gap.txt`. Pass when no request fails.
- [ ] Lane 5. SIGKILL Caddy on a tunnel Machine. Save `holder-keeps-connector.txt`. Pass when the cloudflared restart count stays 0 and the app answers 200 within ten seconds.
- [ ] Lane 6. Stop the tunnel container on one of two Machines. Save `connector-failover.txt`. Pass when the app keeps answering 200 within ten seconds.
- [ ] Lane 7. As a normal Service, bind-mount the tunnel token directory and cat it. Save `token-reach.txt`. Pass when the token file holds no plaintext outside the tunnel container's tmpfs. Record the result either way, since Operator scope can still run any container.
- [ ] Lane 8. Run `add`, giving a third Machine the ingress role, on a tunnel Cluster. Save `tunnel-catch-up.txt`. Pass when the joiner runs the holder Closed, Caddy, and a connected cloudflared.
- [ ] Lane 9. Run `server set --ingress off` on the last tunnel Machine. Save `last-door.txt`. Pass when the CLI asks for confirmation and names the last open door.
- [ ] Lane 10. Watch the certificate run on a tunnel Cluster for one cycle. Save `acme-skip.txt`. Pass when no HTTP-01 challenge is attempted and the run records the refusal reason.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. p50 and p99 latency of 500 requests from the same client through the Public door and through the tunnel. Trunk has only the Public door. Also count failed requests during a public to tunnel switch.
- [ ] Probe. Five interleaved rounds of 100 requests each to trunk Public, head Public, and head tunnel, then three switches with a 200 ms curl loop.
- [ ] Baseline. Record the trunk Public p50 and p99 first.
- [ ] Rule. Head Public within 10% of trunk. Head tunnel p50 at or under trunk Public p50 plus 150 ms. Zero failed requests across switches. Any miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3, and 9 transcripts into `/tmp/cluster-ingress-review/E2-review-<slug>.png` as terminal screenshots.
- [ ] Record a 30 to 60 second terminal video of `ployz ingress use tunnel` and back on a lane VM. Save it as `/tmp/cluster-ingress-review/E2-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto its parent branch after the verdict, patch-id unchanged.
- [ ] The root hands E2 to the operator at verified. The operator lands it after E1.

## Name the Cluster and add the ployz.internal alias (E3)

**Depends on.** None.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/value.rs`.
- [ ] Edit `core/crates/ployz-core/tests/contract.rs`.
- [ ] Edit `core/crates/ployz-core/src/rpc_catalog.rs` and `core/crates/ployz-core/src/rpc.rs`.
- [ ] Edit `core/crates/ployz-core/tests/capabilities.rs`.
- [ ] Edit `core/crates/ployzd/src/corrosion/store.rs` and `core/crates/ployzd/src/corrosion/fake_cluster.rs`.
- [ ] Edit `core/crates/ployzd/src/dns/query.rs` and `core/crates/ployzd/src/dns/service.rs`.
- [ ] Edit `core/crates/ployzd/src/machine_api/local.rs`.
- [ ] Edit `core/crates/ployz/src/handlers/mod.rs`.
- [ ] Create `core/crates/ployz/src/handlers/cluster.rs`.
- [ ] Edit `core/crates/ployz/src/sdk.rs`, `core/crates/ployz-sdk/src/lib.rs`, and `core/crates/ployz/src/approval.rs`.
- [ ] Edit `core/CONTEXT.md`.
- [ ] Edit `docs/user/services/private-networking.md`.

**Build.**

- [ ] Add `ployz` to `Namespace::is_reserved`. Cloud and the CLI can no longer create it.
- [ ] Add the `ClusterName` newtype, a DNS label of at most 63 characters. Store it as the optional `cluster` key `name`, read and written like `sealing_public_key`.
- [ ] Add the route `SetClusterName` with capability `ployz.cluster.name.v1`, using the E1 route recipe.
- [ ] Thread the Cluster Name into `dns::query::parse`. Strip `.<cluster>.ployz.internal.` and reuse `parse_internal`. Add `Query::NxDomain` for a foreign or unset cluster label and map it in `dns/service.rs`.
- [ ] Add `--cluster-name` to `server add --standalone` and `--token-stdin`. A client founding a Cluster requires it. With `--json` and no name, fail with `invalid_argument` naming the flag.
- [ ] Add `ployz cluster name [<name>]` to read and set it.
- [ ] Add the `PloyzSession` method `set_cluster_name`, its napi binding, and the Rust approval `Verb` `setClusterName`.
- [ ] Add the term Cluster Name to `core/CONTEXT.md`, and reserve `ployz` under Namespace.
- [ ] Document `<svc>.<ns>.<cluster>.ployz.internal` in `docs/user/services/private-networking.md`.
- [ ] Name bets 8 and 9 and the Stable promise in the PR body.

**You see.**

- [ ] `dig @<gateway> postgres.prod.acme.ployz.internal` answers the Service address on a Cluster named `acme`.
- [ ] The same query with `other` in place of `acme` answers NXDOMAIN.
- [ ] `ployz deploy` into Namespace `ployz` fails with `reserved namespace`.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `contract` gains `ployz` as reserved. Run `cargo test -p ployz-core --test contract` in `core/`.
- [ ] `value` gains `ClusterName` parsing at 63 and refusal at 64. Run `cargo test -p ployz-core --lib value` in `core/`.
- [ ] `dns` gains the four-label alias, foreign NXDOMAIN, and unset NXDOMAIN. Run `cargo test -p ployzd --lib dns` in `core/`.
- [ ] `ployz-store` refuses Namespace `ployz`. Run `cargo test -p ployz-store deployment` in `core/`.
- [ ] `cli_shape` gains `cluster name` and `--cluster-name`. Run `cargo test -p ployz --test cli_shape` in `core/`.
- [ ] `capabilities.rs` gains `ployz.cluster.name.v1`. Run `cargo test -p ployz-core --test capabilities` in `core/`.
- [ ] The internal DNS cluster test passes. Run `PLOYZ_TESTKIT_IMAGE=$(scripts/testkit-image.sh ployz-testkit-checkout:dev) cargo test -p ployz --test internal_dns_cluster -- --ignored --test-threads=1` in `core/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Resolve `postgres.prod.ployz.internal` and `postgres` from a Service at trunk and head. Save `short-forms.txt`. Pass when both forms answer the same address on both sides.
- [ ] Lane 2. Name the Cluster `acme` and resolve the four-label alias from a Service. Save `alias.txt`. Pass when it answers the Service address.
- [ ] Lane 3. Resolve the alias with a foreign cluster label. Save `foreign.txt`. Pass when it answers NXDOMAIN.
- [ ] Lane 4. Resolve the alias on an unnamed Cluster. Save `unnamed.txt`. Pass when it answers NXDOMAIN and the short forms still answer.
- [ ] Lane 5. Deploy into Namespace `ployz`. Save `reserved.txt`. Pass when the deploy fails with `reserved namespace`.
- [ ] Lane 6. Found a Cluster with `server add --standalone --json` and no name. Save `founding-json.txt`. Pass when it fails with `invalid_argument` naming `--cluster-name`.
- [ ] Lane 7. Set a 64-character name. Save `name-cap.txt`. Pass when it fails and names the 63-character cap.
- [ ] Lane 8. Rename the Cluster from `acme` to `beta`. Save `rename.txt`. Pass when the `beta` alias answers and the `acme` alias answers NXDOMAIN within one minute.
- [ ] Lane 9. Run `add` on a named Cluster. Save `joiner-name.txt`. Pass when the joiner resolves the alias.
- [ ] Lane 10. Run a trunk CLI against the head Cluster and run `ployz status`. Save `old-cli.txt`. Pass when it works and ignores the name.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. p50 and p99 query time of the short form at trunk and head, and of the alias at head, which trunk lacks.
- [ ] Probe. `dnsperf` or a 1000-query `dig` loop from a Service, five interleaved rounds per side.
- [ ] Baseline. Record the trunk short-form p50 and p99 first.
- [ ] Rule. Head short form within 10% of trunk. Alias p50 at or under two milliseconds. Either miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3, and 6 transcripts into `/tmp/cluster-ingress-review/E3-review-<slug>.png` as terminal screenshots.
- [ ] Record a 30 to 60 second terminal video of `ployz cluster name acme` and a `dig` of the alias on a lane VM. Save it as `/tmp/cluster-ingress-review/E3-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root hands E3 to the operator at verified. The operator lands it.

## Run Tailscale as a reserved subnet router (E4)

**Depends on.** E2.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/value.rs`.
- [ ] Create `core/crates/ployz-core/src/domain/tailscale.rs`.
- [ ] Edit `core/crates/ployz-core/src/sealed.rs`.
- [ ] Edit `core/crates/ployz-core/src/service/eligibility.rs`.
- [ ] Edit `core/crates/ployz-core/src/rpc_catalog.rs`, `core/crates/ployz-core/src/rpc.rs`, and `core/crates/ployz-core/tests/capabilities.rs`.
- [ ] Edit `core/crates/ployzd/src/images.rs` and `core/crates/ployzd/src/machine_api/local.rs`.
- [ ] Edit `core/crates/ployz/src/global_catch_up.rs`.
- [ ] Create `core/crates/ployz/src/handlers/private_access.rs`.
- [ ] Edit `core/crates/ployz/src/sdk.rs`, `core/crates/ployz-sdk/src/lib.rs`, and `core/crates/ployz/src/approval.rs`.
- [ ] Edit `core/CONTEXT.md`.
- [ ] Edit `docs/user/services/private-networking.md`.

**Build.**

- [ ] Add `QualifiedService::system_tailscale()` and its canonical Global spec. It runs the pinned tailscale image with `TS_USERSPACE=true`, advertises the Cluster network as a subnet route, and reads its auth key from a tmpfs.
- [ ] Add the sealing purpose `private-access` on E2's platform context.
- [ ] Add a reserved branch to `placement_eligibility` so the router runs on every Machine, ingress-only Machines included.
- [ ] Add the route `SetPrivateAccess { enabled, sealed_auth_key }` with capability `ployz.private-access.v1`. Global catch-up calls it on a joiner.
- [ ] Add `ployz private-access on --auth-key-stdin` and `ployz private-access off`. The `on` output tells the user to approve the route in the Tailscale admin and to run `tailscale up --accept-routes` on Linux.
- [ ] Add the `PloyzSession` method `set_private_access`, its napi binding, and the Rust approval `Verb` `setPrivateAccess`.
- [ ] Add the term Private Access to `core/CONTEXT.md`.
- [ ] Add a Tailscale section to `docs/user/services/private-networking.md`.
- [ ] Name bets 4, 6, and 10 and the Stable promise in the PR body.

**You see.**

- [ ] `ployz private-access on --auth-key-stdin` prints one router per Machine and the route approval hint.
- [ ] From a laptop on the tailnet, `psql -h postgres.prod.acme.ployz.internal` connects.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `eligibility` gains a case that the router is eligible on an ingress-only Machine. Run `cargo test -p ployz-core --lib service::eligibility` in `core/`.
- [ ] `sealed` gains a round trip for `private-access`. Run `cargo test -p ployz-core --lib sealed` in `core/`.
- [ ] `capabilities.rs` gains `ployz.private-access.v1`. Run `cargo test -p ployz-core --test capabilities` in `core/`.
- [ ] `machine_api::local_tests` gains on, off, and a missing key. Run `cargo test -p ployzd --lib machine_api::local_tests` in `core/`.
- [ ] `cli_shape` gains `private-access on` and `off`. Run `cargo test -p ployz --test cli_shape` in `core/`.
- [ ] `approval` stays green with `setPrivateAccess`. Run `cargo test -p ployz --lib approval` in `core/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Run Tailscale as a user Service with `replicas=2` at trunk, as the design's verify run did, and the reserved router at head. Connect to Postgres from a tailnet client on both. Save `postgres.txt`. Pass when both sides connect.
- [ ] Lane 2. Send UDP to an echo Service over the tailnet. Save `udp.txt`. Pass when every packet echoes.
- [ ] Lane 3. Resolve the alias and the short form from the tailnet client through the Cluster DNS. Save `dns.txt`. Pass when both answer.
- [ ] Lane 4. Stop the router serving the client's route. Save `failover.txt`. Pass when Postgres reconnects within 30 seconds.
- [ ] Lane 5. Run `add` on a Cluster with private access on. Save `joiner.txt`. Pass when the joiner runs a router that appears in the tailnet.
- [ ] Lane 6. Run `private-access off`. Save `off.txt`. Pass when every router is gone and the tailnet client can no longer connect.
- [ ] Lane 7. Run `private-access on` with a revoked key. Save `bad-key.txt`. Pass when the CLI reports the router's auth failure per Machine.
- [ ] Lane 8. Run the router on an ingress-only Machine added with `add --roles ingress`. Save `ingress-only.txt`. Pass when that Machine runs a router.
- [ ] Lane 9. Grep `commands.jsonl`, the journal, and `docker inspect` for the auth key. Save `key-leak.txt`. Pass when no plaintext key appears.
- [ ] Lane 10. Restart the daemon on one Machine. Save `daemon-restart.txt`. Pass when the router keeps running with restart count 0 and the client stays connected.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Idle router RSS and CPU per Machine, and seconds of Postgres failover, at trunk with the user Service and at head with the reserved router.
- [ ] Probe. `docker stats --no-stream` sampled ten times per side, then three failovers per side, interleaved.
- [ ] Baseline. Record the trunk RSS, CPU, and failover median first. The design's run measured about 16 seconds.
- [ ] Rule. Head RSS and CPU within 10% of trunk. Head failover median at or under trunk plus five seconds. Either miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 1, 2, and 4 transcripts into `/tmp/cluster-ingress-review/E4-review-<slug>.png` as terminal screenshots.
- [ ] Record a 30 to 60 second terminal video of `ployz private-access on` and a `psql` connect from the tailnet. Save it as `/tmp/cluster-ingress-review/E4-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto its parent branch after the verdict, patch-id unchanged.
- [ ] The root hands E4 to the operator at verified. The operator lands it after E2.

## Add Settings, Network with the Cluster Name (C1)

**Depends on.** E3.

**Files.**

- [ ] Edit `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/settings.tsx`.
- [ ] Create `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/-components/NetworkSection.tsx`.
- [ ] Move `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection.tsx` under the Network section.
- [ ] Edit `dashboard/src/modules/approvals/approvals.ts`, `dashboard/src/modules/operations/operations.ts`, and `dashboard/src/modules/operations/operations.server.ts`.
- [ ] Edit `dashboard/src/modules/runtime/ployz.server.ts`.
- [ ] Edit `dashboard/src/modules/machines/waiting/enroll.server.ts`.
- [ ] Edit `dashboard/CONTEXT.md`.
- [ ] Edit `docs/user/account/` and `docs/user/services/private-networking.md`.

**Build.**

- [ ] Add a Network section to Settings that holds the Cluster Name row and the existing Cluster Domain section.
- [ ] Add `setClusterName` to `OperationVerb` on its one line, to `OPERATIONS`, and to its `gate`, modelled on `setServer`. A rename from the CLI or an agent asks for an Approval. The dashboard's own Save acts directly, per the Approval rule in `dashboard/CONTEXT.md`.
- [ ] Default the Cluster Name to the Organization slug cut to 63 characters, and pass `--cluster-name` in the founding enroll command.
- [ ] Add the term Cluster Name to `dashboard/CONTEXT.md`.
- [ ] Document where to rename a Cluster.
- [ ] Name bets 6 and 8 in the PR body.

**You see.**

- [ ] Settings shows Network with `Cluster name acme` and an Edit button.
- [ ] Edit, then Save, renames the Cluster and the row shows the new name. The same rename through `ployz mcp` shows an Approval card.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] An operations unit test gains `setClusterName` gating. Run `pnpm exec vitest run --project unit operations` in `dashboard/`.
- [ ] An enroll postgres test gains `--cluster-name` in the founding command and a 63-character cut. Run `pnpm exec vitest run --project postgres enroll` in `dashboard/`.
- [ ] Types and lint pass. Run `pnpm typecheck && pnpm lint` in `dashboard/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Open Settings and the Cluster Domain section at trunk and head. Save `settings.png`. Pass when the Cluster Domain section shows the same domain on both sides.
- [ ] Lane 2. Open Network on a new Organization. Save `name-default.png`. Pass when the name shows the slug.
- [ ] Lane 3. Rename from Settings. Save `rename.png`. Pass when the row shows the new name with no Approval and `ployz cluster name` on a Machine agrees.
- [ ] Lane 4. Ask the Agent sidebar to rename, then deny its Approval card. Save `rename-agent-denied.png`. Pass when the row keeps the old name.
- [ ] Lane 5. Enter a 64-character name. Save `name-cap.png`. Pass when the form refuses it and names the cap.
- [ ] Lane 6. Enter a name with uppercase and spaces. Save `name-invalid.png`. Pass when the form names the DNS label rule.
- [ ] Lane 7. Enroll the first Server on an Organization with a 70-character slug. Save `long-slug.png`. Pass when the Cluster Name is the 63-character cut.
- [ ] Lane 8. Open Network at phone width. Save `mobile.png`. Pass when nothing scrolls horizontally.
- [ ] Lane 9. Open Network as a non-admin member. Save `member.png`. Pass when Edit is not offered.
- [ ] Lane 10. Rename while the only Server is offline. Save `offline.png`. Pass when the row says the change is pending until the Server returns.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Settings route loader time and LCP at trunk and head.
- [ ] Probe. Ten interleaved loads per side with agent-browser reading `performance.getEntriesByType`.
- [ ] Baseline. Record the trunk median first.
- [ ] Rule. Head median at or under trunk plus 100 ms. A miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3, and 5 screenshots into `/tmp/cluster-ingress-review/C1-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of a rename from Settings and one from the Agent sidebar on a lane VM. Save it as `/tmp/cluster-ingress-review/C1-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root hands C1 to the operator at verified. The operator lands it after E3.

## Switch a Cluster to Cloudflare and back from Settings (C2)

**Depends on.** E2 and C1.

**Files.**

- [ ] Create `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/-components/WebTrafficSection.tsx`.
- [ ] Create `dashboard/src/modules/cloudflare/setup.server.ts`.
- [ ] Create `dashboard/src/modules/ingress/doors.inngest.ts`, following `dashboard/src/modules/machines/server-web-traffic.inngest.ts`.
- [ ] Edit `dashboard/src/modules/config-store/domain-evidence.server.ts` and `dashboard/src/modules/cluster-domain/hosted-dns.server.ts`.
- [ ] Edit `dashboard/src/modules/approvals/approvals.ts`, `dashboard/src/modules/operations/operations.ts`, and `dashboard/src/modules/operations/operations.server.ts`.
- [ ] Edit `core/crates/ployz-store/src/review/approval.rs` only if the Destructive Effect needs a new `DestructiveKind`, as a separate engine commit inside the stack.
- [ ] Edit `dashboard/src/modules/runtime/ployz.server.ts`.
- [ ] Edit `dashboard/CONTEXT.md`.
- [ ] Edit `docs/user/services/cloudflare-tunnel.md` and `docs/user/services/domains.md`.

**Build.**

- [ ] Add a Web traffic section with two choices, Public ports and Cloudflare Tunnel. The Cluster is all in on one.
- [ ] Choosing Cloudflare asks for a Cloudflare API token, lists its zones, and suggests a dedicated zone. A server action uses the token once to create a locally configured tunnel and the `*.<zone>` wildcard CNAME, seals the tunnel token to the Cluster Sealing Key, and forgets the API token. The token never enters an Inngest event or step, because those persist.
- [ ] The switch is one review sheet with a Switch button, like the existing web traffic dialog. From the CLI or an agent, the same switch asks for an Approval. The review shows the Destructive Effect that every generated hostname moves from the Hosted domain to the zone, then calls `setIngressDoors` with `{Public, Tunnel}`, waits for every connector, then `{Tunnel}`.
- [ ] Choosing Public reverses it. Doors go `{Public, Tunnel}` then `{Public}`, the source goes back to Hosted, and the user is told the tunnel and wildcard are left in their Cloudflare account to delete.
- [ ] Set the Cluster Domain source to Cloudflare with the zone. Generated names become `<svc>-<env>.<zone>`, one level below the zone, through the expansion in `ployz-store/src/domain/mod.rs`.
- [ ] Add `setIngressDoors` to `OperationVerb`, `OPERATIONS`, and `gate`.
- [ ] Reword Cluster Domain in `dashboard/CONTEXT.md` to cover both sources.
- [ ] Document the dashboard path in `docs/user/services/cloudflare-tunnel.md`.
- [ ] Name bets 6, 7, and 8 in the PR body. Say that Cloud holds no Cloudflare credential after setup.

**You see.**

- [ ] Settings, Network, Web traffic shows Public ports selected and Cloudflare Tunnel offered.
- [ ] After setup and the review, the section shows `Cloudflare Tunnel on zenploy-apps.dev, 2 of 2 servers connected` and a generated hostname answers through Cloudflare.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] A setup unit test gains the zone-depth rule and the wildcard-only record set with a mocked Cloudflare API. Run `pnpm exec vitest run --project unit cloudflare` in `dashboard/`.
- [ ] A postgres test proves no Cloudflare API token is stored in any table or Inngest payload. Run `pnpm exec vitest run --project postgres cloudflare` in `dashboard/`.
- [ ] An attempt test gains the two-step door switch. Run `pnpm exec vitest run --project unit doors` in `dashboard/`.
- [ ] Types and lint pass. Run `pnpm typecheck && pnpm lint` in `dashboard/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Deploy an app with a generated hostname at trunk and head and open it. Trunk lacks Web traffic, so gate the head switch to Cloudflare and the app answering through it. Save `regression.png`. Pass when both sides answer on Public and head answers through Cloudflare after the switch.
- [ ] Lane 2. Run Cloudflare setup on zenploy.dev and confirm the review. Save `setup-done.png`. Pass when every Server shows connected and no Server publishes 80 or 443.
- [ ] Lane 3. Open the review before confirming. Save `destructive-effect.png`. Pass when it lists every generated hostname that moves.
- [ ] Lane 4. Switch back to Public and confirm. Save `back-to-public.png`. Pass when the app answers on the Hosted domain and the section says what to delete in Cloudflare.
- [ ] Lane 5. Paste a token without DNS edit permission. Save `weak-token.png`. Pass when setup names the missing permission and changes nothing.
- [ ] Lane 6. Pick a subdomain as the zone. Save `zone-depth.png`. Pass when the form refuses it and explains the one-level rule.
- [ ] Lane 7. Ask the Agent sidebar to switch to Cloudflare and deny its Approval card. Save `agent-denied.png`. Pass when no door changes.
- [ ] Lane 8. Curl the zone apex and an existing explicit record after setup. Save `apex-untouched.png`. Pass when both answer as before setup.
- [ ] Lane 9. Search the database dump and Inngest runs for the API token. Save `token-forgotten.png`. Pass when it appears nowhere.
- [ ] Lane 10. Run setup at phone width. Save `mobile.png`. Pass when every step fits with no horizontal scroll.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Settings route load at trunk and head, and the setup server action's wall time and the switch's end-to-end time to all connectors at head, which trunk lacks.
- [ ] Probe. Ten interleaved Settings loads per side, then three setups and switches at head.
- [ ] Baseline. Record the trunk Settings median first.
- [ ] Rule. Head Settings at or under trunk plus 100 ms. Setup action at or under ten seconds. Switch end to end at or under 90 seconds. Any miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3, and 4 screenshots into `/tmp/cluster-ingress-review/C2-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of Cloudflare setup through the review on a lane VM. Save it as `/tmp/cluster-ingress-review/C2-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto its parent branch after the verdict, patch-id unchanged.
- [ ] The root hands C2 to the operator at verified. The operator lands it after E2 and C1.

## Check custom domains against the tunnel in Cloudflare mode (C3)

**Depends on.** C2.

**Files.**

- [ ] Edit `dashboard/src/modules/config-store/domain-evidence.server.ts`.
- [ ] Edit the domain sheet under `dashboard/src/routes/_protected/cloud/$organizationSlug/` that shows DNS instructions.
- [ ] Edit `docs/user/services/domains.md` and `docs/user/troubleshooting/app-not-reachable.md`.

**Build.**

- [ ] In Cloudflare mode, skip the public A and AAAA probes against Machine IPs. A custom hostname inside the zone is covered by the wildcard. A hostname elsewhere passes when it CNAMEs to `<tunnel id>.cfargotunnel.com`.
- [ ] Show the CNAME target, not Server IPs, in the domain sheet.
- [ ] Document both cases in `domains.md`, and a tunnel row in `app-not-reachable.md`.
- [ ] Name bet 7 in the PR body.

**You see.**

- [ ] Adding `shop.example.org` shows `CNAME shop to <id>.cfargotunnel.com` and turns green once that record exists.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] A domain evidence test gains in-zone, out-of-zone CNAME, and wrong-target cases. Run `pnpm exec vitest run --project unit domain-evidence` in `dashboard/`.
- [ ] Types and lint pass. Run `pnpm typecheck && pnpm lint` in `dashboard/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Add a custom domain on a Public Cluster at trunk and head. Save `public-domain.png`. Pass when both show Server IPs and verify.
- [ ] Lane 2. Add an in-zone hostname in Cloudflare mode. Save `in-zone.png`. Pass when it verifies with no DNS change.
- [ ] Lane 3. Add an out-of-zone hostname and create its CNAME. Save `out-of-zone.png`. Pass when the sheet shows the tunnel target and turns green.
- [ ] Lane 4. Point the CNAME at the wrong target. Save `wrong-target.png`. Pass when the sheet names the expected target.
- [ ] Lane 5. Add a two-level in-zone name. Save `two-level.png`. Pass when the sheet warns that the free certificate does not cover it.
- [ ] Lane 6. Remove a verified hostname. Save `remove.png`. Pass when the route goes away and Cloudflare is untouched.
- [ ] Lane 7. Switch back to Public with a custom domain set. Save `switch-back.png`. Pass when the sheet shows Server IPs again.
- [ ] Lane 8. Open the domain sheet at phone width. Save `mobile.png`. Pass when the CNAME row is copyable and fits.
- [ ] Lane 9. Curl the verified out-of-zone hostname. Save `serves.png`. Pass when it answers 200.
- [ ] Lane 10. Check domain evidence logs. Save `no-ip-probe.png`. Pass when no A or AAAA probe to a Machine IP runs in Cloudflare mode.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Domain evidence run time per hostname at trunk on Public and at head on both modes.
- [ ] Probe. Twenty interleaved evidence runs per side.
- [ ] Baseline. Record the trunk median first.
- [ ] Rule. Head on either mode at or under trunk plus 200 ms. A miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3, and 4 screenshots into `/tmp/cluster-ingress-review/C3-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of adding an out-of-zone hostname on a lane VM. Save it as `/tmp/cluster-ingress-review/C3-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto its parent branch after the verdict, patch-id unchanged.
- [ ] The root hands C3 to the operator at verified. The operator lands it after C2.

## Turn on Tailscale from Integrations (C4)

**Depends on.** E4 and C1.

**Files.**

- [ ] Create `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/integrations.tsx` and `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/-components/integrations.tsx`.
- [ ] Edit `dashboard/src/components/dashboard-navigation-model.ts`.
- [ ] Create `dashboard/src/modules/private-access/private-access.inngest.ts`.
- [ ] Edit `dashboard/src/modules/approvals/approvals.ts`, `dashboard/src/modules/operations/operations.ts`, and `dashboard/src/modules/operations/operations.server.ts`.
- [ ] Edit `dashboard/src/modules/runtime/ployz.server.ts`.
- [ ] Edit `dashboard/CONTEXT.md`.
- [ ] Edit `docs/user/services/private-networking.md`.

**Build.**

- [ ] Add an org-level Integrations page with a Tailscale card, plus Cloudflare and GitHub cards that link to their Settings sections. The Tailscale card has Off and Connected. Turning it on asks for an auth key, seals it to the Cluster Sealing Key in a server action, and calls `setPrivateAccess`. From the CLI or an agent it asks for an Approval.
- [ ] Once connected, show each router, the Tailscale admin link to approve the subnet route, and the `--accept-routes` hint for Linux clients.
- [ ] Add `setPrivateAccess` to `OperationVerb`, `OPERATIONS`, and `gate`.
- [ ] Add the term Private Access to `dashboard/CONTEXT.md`.
- [ ] Document the dashboard path in `private-networking.md`.
- [ ] Name bets 4 and 6 in the PR body.

**You see.**

- [ ] The Tailscale card on Integrations shows `Connected, 2 routers` and the link to approve the route in Tailscale.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] An operations test gains `setPrivateAccess` gating. Run `pnpm exec vitest run --project unit operations` in `dashboard/`.
- [ ] A postgres test proves the plaintext key is stored nowhere. Run `pnpm exec vitest run --project postgres private-access` in `dashboard/`.
- [ ] Types and lint pass. Run `pnpm typecheck && pnpm lint` in `dashboard/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Open Settings at trunk and head. Trunk lacks Private access, so gate turning it on and a tailnet Postgres connect. Save `regression.png`. Pass when Settings loads on both and head connects.
- [ ] Lane 2. Turn Tailscale on. Save `on.png`. Pass when the section lists one router per Server.
- [ ] Lane 3. Connect to Postgres from a tailnet client. Save `connect.png`. Pass when it connects.
- [ ] Lane 4. Turn it off. Save `off.png`. Pass when no router runs.
- [ ] Lane 5. Paste a malformed key. Save `bad-key.png`. Pass when the form refuses it before anything runs.
- [ ] Lane 6. Paste a revoked key. Save `revoked-key.png`. Pass when the section shows the auth failure per Server.
- [ ] Lane 7. Ask the Agent sidebar to turn Tailscale on and deny its Approval card. Save `agent-denied.png`. Pass when nothing changes.
- [ ] Lane 8. Search the database dump and Inngest runs for the key. Save `key-forgotten.png`. Pass when it appears nowhere.
- [ ] Lane 9. Open the section at phone width. Save `mobile.png`. Pass when it fits.
- [ ] Lane 10. Add a Server while Tailscale is on. Save `new-server.png`. Pass when the section lists its router with no further step.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Settings route load at trunk and head, and time from Connect to all routers listed at head, which trunk lacks.
- [ ] Probe. Ten interleaved Settings loads per side, then three enables at head.
- [ ] Baseline. Record the trunk Settings median first.
- [ ] Rule. Head Settings at or under trunk plus 100 ms. Enable end to end at or under 90 seconds. Either miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3, and 6 screenshots into `/tmp/cluster-ingress-review/C4-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of turning Tailscale on from Integrations on a lane VM. Save it as `/tmp/cluster-ingress-review/C4-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto its parent branch after the verdict, patch-id unchanged.
- [ ] The root hands C4 to the operator at verified. The operator lands it after E4 and C1.

## Set up a new Cluster from one card (C5)

**Depends on.** C2 and C4.

**Files.**

- [ ] Edit `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/first-run-card.tsx`.
- [ ] Edit `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/servers-page-state.ts`.
- [ ] Edit `dashboard/src/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/BottomBar.tsx` and `AddServersDialog` so the canvas's Add servers opens the same card.
- [ ] Edit `dashboard/src/modules/machines/waiting/enroll.server.ts`.
- [ ] Edit `docs/user/getting-started/` and `docs/user/servers/add-a-server.md`.

**Build.**

- [ ] Keep today's Add your first server card and add one collapsed disclosure, Use Cloudflare Tunnel instead of public ports. Inside it are the Cluster name, defaulted to the slug, and a Cloudflare switch that runs the token, zone, and tunnel steps in the card. Get install command waits until the tunnel is ready. Tailscale is not in onboarding. The prototype is `proto/cluster-onboarding-ui` at `e98345aeaa`, variant A.
- [ ] The install line carries the choices. A Cloudflare choice runs setup first and enrolls the first Server with the holder Closed, so 80 and 443 never open.
- [ ] The canvas's Add servers opens the same card. A user with no Server can still skip it and build a project, as today.
- [ ] Document the card in getting started and in add a server.
- [ ] Name bet 8 in the PR body.

**You see.**

- [ ] A new Organization's Servers page shows today's card with one collapsed Cloudflare line under Get install command.
- [ ] Choosing Cloudflare, then enrolling, gives a first Server that never publishes 80 or 443.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `servers-page-state` gains the setup states. Run `pnpm exec vitest run --project unit servers-page-state` in `dashboard/`.
- [ ] An enroll test gains the Closed first enroll for Cloudflare. Run `pnpm exec vitest run --project postgres enroll` in `dashboard/`.
- [ ] Types and lint pass. Run `pnpm typecheck && pnpm lint` in `dashboard/`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Enroll the first Server with defaults at trunk and head. Save `defaults.png`. Pass when both Servers join as Public with the Hosted domain.
- [ ] Lane 2. Enroll with Cloudflare chosen. Save `cloudflare-first.png`. Pass when the first Server never publishes 80 or 443, checked with `ss -ltn` from the moment it joins.
- [ ] Lane 3. Open and close the disclosure without changing anything, then enroll. Save `untouched-disclosure.png`. Pass when the Server joins as Public with the default Cluster Name.
- [ ] Lane 4. Skip the card and create a project with no Server. Save `project-first.png`. Pass when the canvas works and Deploy reads Add servers.
- [ ] Lane 5. Press Add servers on the canvas. Save `canvas-opens-card.png`. Pass when the same card opens.
- [ ] Lane 6. Edit the name in the card. Save `edit-name.png`. Pass when the founded Cluster carries the edited name.
- [ ] Lane 7. Open the card on an Organization that already has a Server. Save `not-first.png`. Pass when the card is not shown and Settings holds the choices.
- [ ] Lane 8. Open the card at phone width. Save `mobile.png`. Pass when it fits with no horizontal scroll.
- [ ] Lane 9. Abandon Cloudflare setup half way. Save `abandon.png`. Pass when the card falls back to Public and says so.
- [ ] Lane 10. Wait on the card while the Server joins. Save `waiting.png`. Pass when the card turns into the Servers list on join.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Servers page load and LCP for a new Organization at trunk and head.
- [ ] Probe. Ten interleaved loads per side with agent-browser.
- [ ] Baseline. Record the trunk median first.
- [ ] Rule. Head median at or under trunk plus 100 ms. A miss fails.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 1, 2, and 5 screenshots into `/tmp/cluster-ingress-review/C5-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of a new Organization through the card to a joined Server on a lane VM. Save it as `/tmp/cluster-ingress-review/C5-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto its parent branch after the verdict, patch-id unchanged.
- [ ] The root hands C5 to the operator at verified. The operator lands it after C2 and C4.

## Close the program

- [ ] Every box above is checked with its evidence.
- [ ] Reply to the operator with the report the execution playbook names, each PR with its verdict and head SHA.
- [ ] Confirm every zenploy.dev tunnel and record the lanes created is deleted, and every Tailscale key the lanes used is revoked.

## Appendix A. Prototype evidence

Each question below was settled by a run. The run artifacts stayed on the machine that ran them. `docs/agents/design/cluster-ingress/design.md` summarizes them.

- Named-tunnel connectors share one remote config, give no load spread, and fail over in about five seconds when one dies. A dead origin behind a live connector returns 502. An http origin loops on redirect. Two-level wildcards fail on a free zone. Quick tunnels are unusable. This was prototype 1 on zenploy.dev, with every tunnel and record deleted afterwards.
- A connector joined to Caddy's netns strands when Caddy restarts. This was prototype 2.
- A pause holder fixes it. The connector survived a Caddy crash, `docker restart`, and remove and recreate, with zero connector restarts.
- The loopback trust boundary holds. A bridge container could neither reach the loopback listener nor forge `CF-Connecting-IP`.
- Tailscale as a userspace subnet router serves Postgres, UDP, and internal DNS, and fails over in about 16 seconds with no failback. Linux clients need `--accept-routes`. This was a verify-server run on Boat.
- The onboarding shape comes from a prototype in the real dashboard. Variant A keeps today's first-run card and adds one collapsed Cloudflare disclosure. Variant B, a two-choice Set up your cluster card, made every new user read a choice most don't need and tripled the card's height. Tailscale moved to an Integrations page.

Still unproven, and owned by the PR named.

- The older-client compat of the translated Caddy spec. E1 lane 10 proves it.
- Whether the tmpfs token step can reuse the Build secrets path unchanged. E2 owns it.
- Whether `verify-cluster` can add a Machine to a Cloud run. E0 lane 5 proves it.

## Appendix B. Alternatives rejected

- A stored Cluster mode lost to observed doors. Doors need no migration and show the real state during a switch.
- A per-Server door mix lost to all in per Cluster. It doubled the support surface for no named user.
- A new `ContainerKind` for the holder lost to a reserved Service. Older readers reject unknown variants.
- Copying observed specs in catch-up lost to placing reserved Globals only through routes. Copies carry a peer's image pin and break during a rolling upgrade.
- A daemon loop that restarts a stranded connector lost to the holder. A loop cannot run while the daemon is down, which breaks bet 10.
- Cloud holding a Cloudflare API token lost to one-time use. Hostnames under the zone need no further DNS work once the wildcard exists.
- An apex record lost to the wildcard alone. The apex stays the user's.
- Tailscale serve lost to a subnet router. Serve carries only HTTP and TCP per port, and the user wants every protocol.
- Asking at first deploy and defaults only lost to the setup card, per Appendix A.

## Appendix C. Risks

- E1 changes the Ingress Proxy every Cluster runs. A wrong holder spec takes every app offline. The owner watches E1 lanes 1, 4, 5, and 10 and runs the verify-caddy-load model.
- E1 makes older CLIs depend on the daemon's translation. The owner watches lane 10 and says in the PR body that any other reserved door spec is refused.
- E2 delivers a plaintext token to a tmpfs that any Operator container could still bind-mount. Operator can already run any container, so this is not new exposure. The optional cloudflared egress uid rule is left for later. The owner records lane 7 either way.
- E2 and E4 need real external accounts. Cloudflare uses zenploy.dev only. Tailscale keys come through `request_secret` and are revoked after.
- C2 must keep the API token out of Inngest. The owner watches lane 9 and the postgres test.
- Boat sandboxes stop after one hour. Lanes that wait on operator approvals start last.
- E3 reserves Namespace `ployz`. Nobody uses it today, per the operator. The owner greps Cloud for any Namespace named `ployz` before merge.
- The Cluster Name cap is 63 while Organization slugs reach 73. C1 cuts the default and lane 7 proves it.

## Appendix D. Links and reading list

- Read `docs/agents/design/cluster-ingress/design.md` and `docs/agents/design/cluster-ingress/grounding.md` before any PR.
- Read `core/DESIGN.md`, `core/CONTEXT.md`, `core/CODING_STANDARDS.md`, `dashboard/DESIGN.md`, and `dashboard/CONTEXT.md` before editing their side.
- Read `docs/agents/landing.md` for PR mechanics.
- The engine exploration with file pointers and test commands is `docs/agents/design/cluster-ingress/engine-pointers.md`. The dashboard exploration is `docs/agents/design/cluster-ingress/dashboard-pointers.md`.
- E1, E2, and C2 get `<plugin root>/skills/how/SKILL.md` before code, since they cross the most boundaries. Every PR gets `<plugin root>/skills/interrogate/SKILL.md` before it opens.
- The np1 audit thread is 709bcb3d. Its route-signing plan adds a Device signature to `SetIngressDoors`, `SetClusterName`, and `SetPrivateAccess` later.
- Keep the decision trail per `<plugin root>/skills/show-me-your-work/SKILL.md` in `.audit/cluster-ingress-program.trail.md`.
