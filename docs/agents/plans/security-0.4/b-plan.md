# Security 0.4 thread B plan, browser approval sessions, proposals and MCP

Root on np1 must not be able to make a customer Machine act unless a customer key approved it. Thread A gave Machines `ployz_core::authority` (PR #3926 restart and stop, PR #3939 join and enforce) and a per-Machine enforcement switch. Thread B gives the dashboard and MCP a way to produce those approvals from a human's passkey, without a Device key in the browser and without a prompt per click.

The design is a chain. A passkey assertion grants a short-lived browser key a scope (one Cluster, one Environment, a set of action families, at most 15 minutes, no re-delegation). The browser key approves each button press. A Machine checks the press against the grant and the grant against its own trusted keys, and refuses anything else. An MCP proposal is one approval over an ordered list of step digests, so an agent's plan costs one prompt and gets no key.

Six PRs. B1 is the smallest end-to-end slice, the Restart button working with enforcement on. B0 must land first because nothing verifies a passkey a Machine does not trust. The appendices hold the data shapes, the C1 gaps, the multi-Cluster rule, the session-length decision, the residual risks and the prototype evidence. The prototype lives on branch `t3/sec04-b-sketch` and its screenshots in `.audit/b-sketch/`.

## How to read this

One box is one unit of work. Every box names the evidence that checks it. A nested box is a sub-step of the box above it. Check a box only when its evidence exists, a file, a log line, a screenshot, a test run, or a SHA. The body is a how-to. The appendices explain and record.

The program runs `<plugin root>/skills/poteto-mode/playbooks/autopilot-stack.md`. Owners open PRs; the operator lands the stack bottom-up. B1, B2, B3 and B4 change an interaction and stop at merge-ready for the operator's review. B0 and B5 merge on a clean verdict.

Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

Terms follow `core/CONTEXT.md` (Device, Management Client, Scope, Cluster, Machine) and `dashboard/CONTEXT.md` (Approval, Approval card, Agent sidebar, Signed-in Device). "Trusted key" is new and defined in Appendix A. No UI text says "sign" or "signature". The trusted CLI shows no prompts. MCP never gets the session key. No log, test or screenshot prints a secret value.

## Program checklist

### Arm the program

- [ ] State the protocol and this plan to the operator, then stop. Start execution only on the operator's explicit go.
- [ ] Read these at program start. Re-read them at every tick.
  - [ ] `cat <plugin root>/skills/poteto-mode/playbooks/autopilot-stack.md`
  - [ ] `cat <plugin root>/skills/swarm/SKILL.md`
  - [ ] `cat .agents/skills/verify-dashboard/SKILL.md`
  - [ ] `cat .agents/skills/verify-server/SKILL.md`
  - [ ] `cat .agents/skills/verify-mcp-login/SKILL.md`
  - [ ] `cat <plugin root>/skills/poteto-mode/playbooks/opening-a-pr.md`
  - [ ] `cat <plugin root>/skills/principle-prove-it-works/SKILL.md`
  - [ ] `cat <plugin root>/skills/principle-sequence-verifiable-units/SKILL.md`
  - [ ] `cat docs/agents/landing.md`
  - [ ] `cat docs/agents/plans/security-0.4/decisions.md`
- [ ] On the operator's go, arm the audit tick as an hourly schedule_task bound to this thread with the tick prompt below. Never leave the cadence to memory.
- [ ] Use this tick prompt, verbatim. "Re-read the execution playbook from disk. Audit the operation against it and fix drift in this tick. Probe every active lane and judge progress by side effects only. Stand down a stuck lane and dispatch its replacement now. Then post a short status message to the operator in chat only when the audit found a tracked change that no earlier status message reported, such as a PR opened, a code-ready head, a round launched or closed, a verdict, a merge, a stuck agent and the action taken, a blocker added or cleared, or a decision only the operator can make. Name every such change and nothing else. Do not repeat a table, the merged list, or an unchanged blocker. If the audit found none, end the turn with no reply text. Either way, log this tick's row in your decision trail. The row names the items reported, or none."
- [ ] On the operator's hold or stand-down, send every owner a zero-writes order at once.

### Spawn owners

- [ ] Spawn one owner per PR with the full lifecycle the execution playbook names.
- [ ] Follow this dependency graph. Start dependent work only after its parent merges, or base it on the parent branch when the execution playbook stacks.
  - [ ] B0 is first. It branches from `t3/sec04-join-approval` (PR #3939) until that lands on `main`, then rebases onto `main`.
  - [ ] B1 after B0. B2 after B1. B3 after B2. B4 after B3. B5 after B2 and after thread C publishes its deploy statement.
  - [ ] C1 gaps in Appendix B that B cannot close inside `ployz_core::authority` are blockers for the PR named there. The root posts each as a blocker to the operator and does not let the owner work around it with Cloud-side checks.
- [ ] Hold the file boundaries. B0 touches `core/crates/ployz-core/src/authority/**`, `core/crates/ployzd/src/machine/local_machine/**`, `core/crates/ployz/src/handlers/device*.rs`, `dashboard/src/modules/devices/**`, `dashboard/src/modules/passkeys/**`, `dashboard/src/server/auth.server.ts` and `docs/user/servers/manage-servers.md`. B1 touches `core/crates/ployz-core/src/authority/**`, `core/crates/ployz-sdk/**`, `dashboard/src/modules/approval-session/**`, `dashboard/src/modules/service-act/**` and the service drawer under `dashboard/src/routes/**/services/$serviceId/**`. B2 touches `dashboard/src/modules/approval-session/**` and `dashboard/src/modules/service-act/**`. B3 touches `core/crates/ployz-core/src/authority/proposal.rs`, `dashboard/src/modules/proposals/**`, `dashboard/src/modules/approvals/**`, `dashboard/src/modules/agent/**` and `dashboard/src/components/agent-sidebar/**`. B4 touches `core/crates/ployz/src/handlers/mcp.rs`, `dashboard/src/modules/agent/remote-mcp.server.ts` and `docs/user/**`. B5 touches `dashboard/src/modules/approval-session/**`, the deploy bottom bar and settings writers, and `core/crates/ployz-sdk/**`.
- [ ] Hold the review gate. B1, B2, B3 and B4 change an interaction. They wait for the operator's review in chat with screenshots and a video before merge.

### PR mechanics, for every PR

- [ ] Resolve the forge once. Default to `gh`; if `command -v origin` succeeds and Origin can resolve the repository, use `origin pr` for every PR operation. Record any fallback to `gh`. Never require `gt`.
- [ ] Open the PR ready, never draft, per **Opening a PR**. Use the run's built-in PR tool when it has one, else `origin pr create --status open --base <base-branch>` or `gh pr create --base <base-branch>` according to the resolved forge. A stack child targets its parent branch.
- [ ] Name the `core/DESIGN.md` bets the PR touches in its body (6 Machine-local authority, 7 Cloud drives and never owns, 9 lean on proven primitives, 10 client first) and the choice each changed.
- [ ] Run the repo's lint and typecheck once before the PR-facing push (`pnpm typecheck && pnpm lint` in `dashboard/`, `cargo fmt --check` and the touched crates' `cargo clippy` in `core/`). Push with hooks on.
- [ ] Run `/deslop` before each commit and `/no-comments` before review.
- [ ] Triage every Bugbot and security-reviewer comment per `../references/bugbot-triage.md`.
- [ ] Rebase onto current trunk before the code-ready report and babysit. Keep that merge base in fix rounds. Rebase again only at merge prep, on a `git merge-tree` conflict with trunk, on a CI failure that comes from a change on trunk, or when a known concurrent change to the same files lands.

### Verdict and merge, for every PR

- [ ] A full swarm runs once, at the code-ready head. Another full swarm runs only for new behavior, a redesign, or a large new diff. A rebase, a merge-conflict resolution, or a minor fix gets a single verifier at the new head, and a wording-only change gets none. Run each full swarm per `<plugin root>/skills/swarm/SKILL.md`. One gates lane. The ten live lanes from the PR's **Verify, live** block. The perf lane from its **Verify, perf** block. Two or more audit lanes, each with its own focus, that read the diff and the receipts and distrust the PR body. One audit lane always takes the attacker's view and tries the attack path the PR names. The root audits the receipts in the merge-ready report before the verdict.
- [ ] Judge every finding by the execution playbook's blocking rule. The verdict is clean when none blocks. A failure on trunk behavior in a lane marked `Regression lane against trunk.` is recorded against trunk and does not block the PR. A new head gets a fresh verdict, except for results that stay valid under the patch-id rule in `playbooks/shipping.md`.
- [ ] The root appends a verified PR to the stack and the operator lands the stack bottom-up, with the patch-id rule from `playbooks/shipping.md`.

### Boot recipe, for every live lane

Each live lane runs in its own worktree at the PR head. Drive through `verify-dashboard` and `verify-server`.

- [ ] `git fetch origin <head-branch> && git checkout <head SHA>`.
- [ ] Dashboard lanes run `SERVERS=1 VERIFY_IDLE_TIMEOUT=0 scripts/verify/up.sh` from `dashboard/`, wait for `.verify/run/info`, run its two `agent-browser` lines, then add a virtual authenticator with `node .audit/b-sketch/virtual-authenticator.mjs $(agent-browser --session $S get cdp-url)` (copied into the lane worktree, since `.audit/` is not committed). Machine lanes run `scripts/verify-cluster up --daemon checkout --cloud --machines 2` from `core/` and use `verify-cluster cli`, `cli --device` and `exec`.
- [ ] Deliver input only through `agent-browser` clicks and typing, `verify-cluster cli` and `verify-cluster exec`. Read-only diagnostics are `.verify/run/vite.log`, `verify-cluster exec <machine> journalctl -u ployzd`, `cat /var/lib/ployz/restart-approvals.json` on a Machine, and `docker exec <session> psql -U postgres -d ployz_cloud`.
- [ ] Save every screenshot to `/tmp/swarm-<pr-id>/worker-<n>/<slug>.png` and return the paths with the report.

## Register a passkey and enroll it as a trusted key (B0)

Outcome. A person adds a passkey in account settings, an existing Full Device approves it with a short code, and every Machine in the Cluster records the passkey as a trusted key that may approve workflows and nothing else. Attack path. A passkey row in Cloud's database, or a `device_request` row marked approved by hand, gives no Machine a trusted key. Only a Full Device's approval does.

**Depends on.** PR #3939 (`t3/sec04-join-approval`), for `Purpose` and `validity`, and the thread A owner's agreement on the `TrustedKey` record in Appendix B, gap 5.

**Decided 2026-11-07 (gap 5 agreed).** The approve-only `TrustedKey` record stands. It must work for browser-only customers, so add a first-key path that needs no CLI Device: `sudo ployz device approve --passkey CODE` run on any Server's own terminal (local socket, Full) enrolls the passkey, and the installer offers it at the end of install. Later passkeys are approved by a trusted Device or by an existing passkey through its own prompt, never through a live session. Use RustCrypto `p256` for ES256.

**Files.**

- [ ] Create `core/crates/ployz-core/src/authority/trusted_key.rs` with `TrustedKey`, `KeyKind::{Device, Passkey}`, `KeyAuthority::{Manage, ApproveWorkflows}` and `PasskeyPublicKey` (COSE ES256 or EdDSA).
- [ ] Edit `core/crates/ployz-core/src/authority/mod.rs` and `keys.rs` so `verify_signers` reads a trusted-key list, not a Device list.
- [ ] Edit `core/crates/ployzd/src/machine/local_machine/restart_approval.rs` so the enforcement path loads trusted keys from the Machine's own record.
- [ ] Edit `core/crates/ployz/src/handlers/device.rs` to add `ployz device approve --passkey CODE`, which signs a `PasskeyEnrollment` with the Device key and writes the trusted key to every Machine the Device reaches.
- [ ] Create `dashboard/src/modules/passkeys/passkeys.server.ts`, `passkeys.functions.ts` and `tables.ts` (`passkey_request`, the passkey twin of `device_request`, holding the COSE public key, the code and a 24 hour expiry).
- [ ] Edit `dashboard/src/server/auth.server.ts` to add Better Auth's passkey plugin, `rpID` and `origin` from `PLOYZ_CLOUD_URL`.
- [ ] Create `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/settings/passkeys.tsx`.
- [ ] Edit `docs/user/servers/manage-servers.md` to describe adding a passkey and approving it with `ployz device approve --passkey`.

**Build.**

- [ ] `TrustedKey` in `trusted_key.rs` carries `kind`, `authority`, `code` (first 10 base32 characters of the SHA-256 of the public key, as `DeviceCode::of` does) and `added_by: DeviceKey`.
- [ ] `PasskeyEnrollment` in `trusted_key.rs` is a `Statement` under `Purpose::Enforce` naming `cluster`, `key`, `authority: ApproveWorkflows`, `issued_at`, `expires_at` and `signer`. `verify_signers` accepts it from a Full Device only.
- [ ] `verify_signers` in `keys.rs` takes `&[TrustedKey]`. A `KeySignature` whose key is a Device verifies as today. A `KeyAuthority::ApproveWorkflows` key verifies only statements whose `Purpose` is `Workflow`.
- [ ] The Machine persists trusted keys in its own record next to `trusted_machines`, never in the Config Store Cloud writes.
- [ ] `ployz device approve --passkey CODE` reads the `passkey_request` through the existing `/api/cli` relay, shows the code and the account email, asks for confirmation in the terminal, signs the enrollment and sends it to every Machine. The Device key never leaves the CLI.
- [ ] The passkey registration page calls `navigator.credentials.create` with `residentKey: "required"` and `userVerification: "required"`, stores the credential with Better Auth, and shows the enrollment code with the exact command to run.
- [ ] A passkey registered but not yet enrolled shows "Not yet approved by a device" on the page.

**You see.**

- [ ] On a Machine, `ployz device ls` prints the passkey as `passkey <code> approve-only <email>` under the Devices.
- [ ] `journalctl -u ployzd` on the Machine prints `trusted key added kind=passkey code=<code> by=<device code>` once per enrollment.
- [ ] Account settings shows the passkey as "Approved by device <code>" after the command runs, with no page reload needed.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `core/crates/ployz-core/src/authority/trusted_key.rs` tests gain an enrollment signed by an Operator-scope Device that `verify_signers` refuses with `AuthorityRefusal::Mismatch { field: "signer" }`. Run `cargo nextest run -p ployz-core authority::`.
- [ ] `keys.rs` tests gain a `Signed<RestartApproval>` signed directly by a passkey public key as if it were a Device, refused with `Mismatch { field: "signatures" }`.
- [ ] `keys.rs` tests gain an `ApproveWorkflows` key on a `Purpose::Enforce` statement, refused.
- [ ] `dashboard/src/modules/passkeys/passkeys.server.test.ts` gains a request whose code is marked approved by a direct row update and asserts the server function still reports "Not yet approved by a device", because the Machine's answer is the only source. Run `pnpm vitest run src/modules/passkeys`.
- [ ] `dashboard/src/server/auth.server.test.ts` asserts the passkey plugin refuses an origin other than `PLOYZ_CLOUD_URL`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5 effort=medium` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Run `ployz device request`, `ployz device approve`, `ployz device ls` at trunk and head on a two-Machine Cluster. Save `b0-device-flow.png`. Pass when both heads list the same Devices and the head also lists the passkey column.
- [ ] Lane 2. Register a passkey in account settings with the virtual authenticator. Save `b0-register.png`. Pass when the page shows the code and the exact `ployz device approve --passkey <code>` line, and the database row holds a COSE key and no private material.
- [ ] Lane 3. Run the approve command from `verify-cluster cli --device`. Save `b0-approve-terminal.png`. Pass when the terminal shows the code and email, asks for confirmation, and both Machines log `trusted key added`.
- [ ] Lane 4. Reload account settings after the approval. Save `b0-approved.png`. Pass when the passkey reads "Approved by device <code>" with the Device's code.
- [ ] Lane 5. Mark a `passkey_request` row approved with `psql` and never run the command. Save `b0-forged-row.png`. Pass when `ployz device ls` on every Machine lists no passkey and the page still says "Not yet approved by a device".
- [ ] Lane 6. Run the approve command from an Operator-scope Device. Save `b0-operator-refused.png`. Pass when the CLI prints a refusal naming Full scope and no Machine logs `trusted key added`.
- [ ] Lane 7. Enroll, then `ployz device rm <passkey code>` from the Full Device. Save `b0-remove.png`. Pass when every Machine drops the key and the page returns to "Not yet approved by a device".
- [ ] Lane 8. Enroll with one Machine stopped; start it; wait for its catch-up. Save `b0-catchup.png`. Pass when the late Machine logs `trusted key added` after it starts and `device ls` there matches the other.
- [ ] Lane 9. Register a passkey while signed in as Grace (admin) and approve it from Ada's Device. Save `b0-grace.png`. Pass when the trusted key records Grace's email and Ada's Device code.
- [ ] Lane 10. Register with a browser that refuses `navigator.credentials.create` (no authenticator). Save `b0-no-authenticator.png`. Pass when the page says the browser could not add a passkey and shows the SSH Device path as the alternative.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Wall time of `ployz device approve` from confirmation to the last Machine's `trusted key added`, at trunk for a Device approval and at head for a passkey approval, on two Machines.
- [ ] Probe. `time ployz device approve <code>` and `time ployz device approve --passkey <code>` five times each, interleaved, reading the Machine log timestamps.
- [ ] Baseline. Record the trunk Device-approval median first.
- [ ] Rule. The passkey approval median stays within 1.5 times the Device approval median and under 5 seconds absolute.

**Review gate.** None. B0 is not review-gated.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root appends B0 to the base-branch stack and the operator lands it bottom-up.

## Restart a Service through a passkey session (B1)

Outcome. With enforcement on, the dashboard's Restart button works. The first press prompts for the passkey once, the restart runs on the Machine, and the Machine's log names the passkey that approved it. Attack path. Cloud with root on np1 cannot restart a container without a live grant from an enrolled passkey; a replayed envelope is refused by the consumption log; a grant whose scope or expiry was edited after the assertion fails its challenge check.

**Depends on.** B0, PR #3926's `RestartApproval` and `admit_operator_restart`, and Appendix B gaps 1, 2, 3 and 4 (SDK TypeScript types, an approval parameter on `stopContainer` and `startContainer`, the `SessionGrant` type and `Signed<T>` carrying grants).

**Files.**

- [ ] Create `core/crates/ployz-core/src/authority/session.rs` with `SessionGrant`, `GrantBody`, `PasskeyAssertion` and `session::verify`.
- [ ] Edit `core/crates/ployz-core/src/authority/mod.rs` so `Signed<T>` gains `grants: Vec<SessionGrant>` and `verify_signers` accepts a browser key covered by a verified grant.
- [ ] Edit `core/crates/ployz-core/src/authority/restart.rs` so `RestartApproval.signer` becomes `Signer::{Device(DeviceKey), Browser(SigningPublicKey)}`.
- [ ] Edit `core/crates/ployz-sdk/src/agent.rs` so `stop_container` and `start_container` take `approval: Option<Signed<RestartApproval>>` and forward it to the daemon call.
- [ ] Edit `core/scripts/build-cloud-sdk.sh` and `core/crates/ployz-sdk/package.json` to export the ts-rs output of `authority` as `@ployz/sdk/authority`.
- [ ] Create `dashboard/src/modules/approval-session/approval-session.ts` and `approval-prompt.tsx` from the prototype, with the ECDSA fallback removed.
- [ ] Edit `dashboard/src/modules/service-act/service-act.functions.ts` so `actServiceServerFn` requires `approval` for `restart` when the Environment's Machines enforce.
- [ ] Edit `dashboard/src/modules/service-act/service-act.server.ts` so `actOnContainer` passes the envelope to the SDK.
- [ ] Edit `dashboard/src/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceActions.tsx` to prompt before the press and show the session badge.
- [ ] Edit `dashboard/src/routes/_protected/cloud/$organizationSlug/route.tsx` to mount `ApprovalPrompt` once.
- [ ] Edit `docs/user/services/manage-services.md` (or the page that documents Restart) to describe the passkey prompt and the 15 minute window.

**Build.**

- [ ] `GrantBody` in `session.rs` is canonical JSON of `browser_key`, `cluster` (Sealing public key), `environment` (namespace), `machines` (sorted distinct), `families`, `issued_at`, `expires_at` and `delegation: "none"`. `session::verify` checks that `expires_at - issued_at` is at most `validity::MAX_LIFETIME_MILLIS`.
- [ ] `PasskeyAssertion` holds `credential_id`, `authenticator_data`, `client_data_json` and `signature`. `session::verify` checks `clientDataJSON.type == "webauthn.get"`, `challenge == base64url(SHA-256(body))`, `origin` in the Machine's allowed origins, `rpIdHash == SHA-256(rp_id)`, the UV flag, and the signature over `authenticator_data || SHA-256(client_data_json)` with the trusted key's COSE public key (`p256` for ES256, `ed25519-dalek` for EdDSA).
- [ ] `verify_signers` in `keys.rs` accepts a `KeySignature` whose key is not trusted when a grant in `grants` names that key as `browser_key`, the grant verifies against a trusted `ApproveWorkflows` key, the grant covers the statement's Cluster, Environment, Machines and family, and the statement's `expires_at` is at most the grant's.
- [ ] The browser key is a non-extractable WebCrypto Ed25519 key that lives in page memory. `startSession` fails with "Your browser can't hold an approval key here" when Ed25519 is missing, and the page offers the CLI path.
- [ ] `actServiceServerFn` builds `RestartApproval` from the envelope body, attaches the grant, and refuses with `Conflict` when the Environment's Machines enforce and no envelope came. Cloud never holds a key that can produce one.
- [ ] `admit_operator_restart` logs `restart approved by passkey <code> via browser key <code>` with the codes only.
- [ ] The prompt dialog is a shadcn `Dialog` titled "Confirm it's you" with the copy from the prototype. Its action is "Use passkey". No text says "sign".

**You see.**

- [ ] With enforcement on, pressing Restart on `web` opens the dialog; after "Use passkey" the drawer shows the `Restarting` badge, then the container's new start time, and the Machine log shows the passkey line.
- [ ] The drawer header shows `Approved for production · 15 min left`.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `session.rs` tests gain a grant whose `expires_at` was moved later after the assertion, refused with `Mismatch { field: "challenge" }`. Run `cargo nextest run -p ployz-core authority::`.
- [ ] `session.rs` tests gain a grant with a 16 minute window, refused with `Expired`.
- [ ] `session.rs` tests gain a grant asserted with `type: "webauthn.create"`, refused.
- [ ] `keys.rs` tests gain a `RestartApproval` for namespace `fix-api` under a grant for `production`, refused with `Mismatch { field: "environment" }`.
- [ ] `keys.rs` tests gain a `RestartApproval` whose `machines` includes an id the grant lacks, refused with `Mismatch { field: "machines" }`.
- [ ] `restart.rs` tests gain a browser-signed approval admitted once and replayed, refused by the consumption log.
- [ ] `dashboard/src/modules/approval-session/approval-session.test.ts` asserts `canonicalJson` sorts keys, refuses null and refuses negative or fractional numbers, matching `ployz_core::authority::canonical` fixtures copied from the Rust tests. Run `pnpm vitest run src/modules/approval-session`.
- [ ] `approval-session.test.ts` asserts `promptReason` returns `first`, `expired` and `scope` for a missing, an expired and a `fix-api` session when `production` is pressed.
- [ ] `dashboard/src/modules/service-act/service-act.functions.test.ts` asserts a restart without an envelope on an enforcing Environment answers `Conflict`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5 effort=medium` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Restart `web` with enforcement off at trunk and head. Save `b1-enforce-off.png`. Pass when both heads restart with no dialog and the badge reads `Restarting` then clears.
- [ ] Lane 2. Enroll a passkey, turn enforcement on with `ployz device enforce`, press Restart. Save `b1-first-press.png`. Pass when the dialog "Confirm it's you" shows before any request leaves the page (`network requests` shows no `_serverFn` post until "Use passkey").
- [ ] Lane 3. Press "Use passkey", wait for the restart. Save `b1-restarted.png`. Pass when the container's start time on the Machine is newer than the press and the Machine log holds `restart approved by passkey`.
- [ ] Lane 4. Press Restart again within the window. Save `b1-second-press.png`. Pass when no dialog shows and the restart runs.
- [ ] Lane 5. Insert an approved `operation_approvals` row with `psql` and post `actServiceServerFn` without an envelope. Save `b1-forged-row.png`. Pass when Cloud answers `Conflict` and the Machine log shows no restart.
- [ ] Lane 6. Capture an envelope with `network har`, edit its body to another request id, and replay the post. Save `b1-tampered.png`. Pass when the Machine refuses and `restart-approvals.json` records no new consumption.
- [ ] Lane 7. Replay the exact captured envelope. Save `b1-replay.png`. Pass when the Machine refuses with the consumption log reason.
- [ ] Lane 8. Turn enforcement on with no passkey enrolled, press Restart, use the passkey. Save `b1-unenrolled.png`. Pass when the Machine refuses with `NoClusterIdentity` or `Mismatch { field: "signatures" }` and the drawer shows the refusal in the act alert.
- [ ] Lane 9. Restart from `verify-cluster cli --device` with enforcement on. Save `b1-cli.png`. Pass when the CLI restarts with no prompt and the log names the Device, not a passkey.
- [ ] Lane 10. Dark mode of lanes 2 and 4 (`agent-browser set media dark`). Save `b1-dark.png`. Pass when the dialog and the badge use the tokens in `styles.css` and the badge stays readable.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Time from the Restart press to the `Restarting` badge, and from the press to the container's new start time, at trunk (no dialog) and at head within a live session.
- [ ] Probe. `agent-browser` click with `performance.now()` stamps read through `eval`, and the container start time from `docker inspect` on the Machine, ten runs each side, interleaved.
- [ ] Baseline. Record the trunk press-to-badge and press-to-start medians first.
- [ ] Rule. Head press-to-badge within 150 ms of trunk; press-to-start within 1.2 times trunk. The one-time prompt is excluded, and its own budget is under 3 seconds from "Use passkey" to the dialog closing with the virtual authenticator.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3 and 4 screenshots into `docs/agents/plans/security-0.4/media/b1-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of the first press, the restart and the second press on a lane VM. Save it as `docs/agents/plans/security-0.4/media/b1-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root appends B1 to the base-branch stack and the operator lands it bottom-up.

## Cover start, stop, expiry and scope with one session helper (B2)

Outcome. The session from B1 covers start and stop too. The badge counts down, an expired session prompts again with its own copy, a press in another Environment prompts for that Environment, and a Volume deletion always prompts on its own. Attack path. A session for `production` cannot stop a Service in `fix-api`; a session edited in `sessionStorage` to a wider scope or a later expiry fails on the Machine because the grant's challenge no longer matches.

**Depends on.** B1.

**Files.**

- [ ] Edit `dashboard/src/modules/approval-session/approval-session.ts` to add `familyOf` for every `ServiceVerb`, `promptReason`, `expireAt` and the `one-at-a-time` family for Volume work.
- [ ] Edit `dashboard/src/modules/approval-session/approval-prompt.tsx` to carry the `expired`, `scope` and `destructive` copy and the `ApprovalSessionBadge`.
- [ ] Edit `dashboard/src/modules/service-act/service-act.functions.ts` so `start` and `stop` require envelopes under enforcement.
- [ ] Edit the Volume drawer's delete path under `dashboard/src/routes/**/volumes/**` so it calls `ensureApproval(scope, "one-at-a-time")` and the confirm dialog reads what is lost.
- [ ] Edit `docs/user/services/manage-services.md` and `docs/user/volumes.md` to say which actions share the window and which ask every time.

**Build.**

- [ ] `promptReason` in `approval-session.ts` returns `first`, `expired`, `scope` or null, and `destructive` for the `one-at-a-time` family, which never reads the session.
- [ ] `ApprovalSessionBadge` reads `Approved for <environment> · <n> min left` (success), `Approval ended` (warning) when past expiry, and `Approved for <other> only` (outline) in another Environment. Hover shows `Approved with your passkey, key <code>`.
- [ ] The dialog copy for `expired` is "Your 15 minutes for <environment> ended. Use your passkey to approve the next 15." For `scope` it is "Your approval covers <other> only. Use your passkey to approve <environment> for the next 15 minutes." For `destructive` it names the loss, as `ConfirmDialog` already does, and adds "Use your passkey to approve this one change."
- [ ] A new session replaces the old one. There is never more than one live session per tab.
- [ ] The session ends when the tab closes, because the browser key lives in page memory. `sessionStorage` holds only the public fields for the badge.

**You see.**

- [ ] After 15 minutes the badge reads `Approval ended` and the next Restart opens the dialog with the `expired` copy.
- [ ] Opening `fix-api` and pressing Restart opens the dialog with the `scope` copy while the badge reads `Approved for production only`.
- [ ] Deleting `pg-data` opens its own dialog naming the loss, even inside a live session.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `approval-session.test.ts` asserts a session whose `sessionStorage` copy was edited to another Environment still produces envelopes for the original grant, so the Machine refuses the press. Run `pnpm vitest run src/modules/approval-session`.
- [ ] `approval-session.test.ts` asserts `promptReason` is `destructive` for the `one-at-a-time` family inside a live session.
- [ ] `approval-session.test.ts` asserts `expireAt` past now flips `promptReason` to `expired` and the badge text to `Approval ended`.
- [ ] `keys.rs` tests gain a `RestartApproval` with kind `Stop` under a grant whose families lack `stop`, refused with `Mismatch { field: "families" }`. Run `cargo nextest run -p ployz-core authority::`.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5 effort=medium` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Stop and start `web` with enforcement off at trunk and head. Save `b2-enforce-off.png`. Pass when both heads act with no dialog.
- [ ] Lane 2. Restart, then Stop, then Start within one session. Save `b2-three-presses.png`. Pass when one dialog shows for the three presses and all three run on the Machine.
- [ ] Lane 3. Start a session, run `expireSessionIn(1000)` through `eval` (test hook), wait, press Restart. Save `b2-expired.png`. Pass when the badge reads `Approval ended` and the dialog shows the `expired` copy.
- [ ] Lane 4. Start a session in `production`, open `fix-api`, press Restart. Save `b2-scope.png`. Pass when the dialog shows the `scope` copy naming both Environments and the first session is replaced after "Use passkey".
- [ ] Lane 5. Inside a live session delete `pg-data`. Save `b2-volume.png`. Pass when a dialog names what is lost and the Machine log names the passkey for that one change.
- [ ] Lane 6. Edit `sessionStorage` to widen `families` and move `expiresAt`, press Stop. Save `b2-tampered-storage.png`. Pass when the Machine refuses and the drawer shows the refusal.
- [ ] Lane 7. Close the tab, open the page again, press Restart. Save `b2-new-tab.png`. Pass when the dialog shows the `first` copy.
- [ ] Lane 8. Cancel the dialog. Save `b2-cancel.png`. Pass when nothing is sent and the drawer is unchanged.
- [ ] Lane 9. Sign in as Grace (admin) in a second session and press Restart while Ada's session is live. Save `b2-grace.png`. Pass when Grace gets her own dialog and her own badge, and the Machine log names Grace's passkey.
- [ ] Lane 10. Phone width (390 px) of lanes 2 and 3. Save `b2-phone.png`. Pass when the badge and the dialog fit without horizontal scroll.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Time from a Stop press to the `Stopping` badge within a live session, at trunk and head.
- [ ] Probe. `agent-browser` click with `performance.now()` stamps through `eval`, ten runs each side, interleaved.
- [ ] Baseline. Record the trunk median first.
- [ ] Rule. Head within 150 ms of trunk.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3, 4 and 5 screenshots into `docs/agents/plans/security-0.4/media/b2-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of expiry, the other Environment and the Volume dialog on a lane VM. Save it as `docs/agents/plans/security-0.4/media/b2-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root appends B2 to the base-branch stack and the operator lands it bottom-up.

## Stage, present and run an agent proposal (B3)

Outcome. An agent stages several steps as a draft. The person sees one card in the Agent sidebar listing the steps in order, approves with one passkey prompt, and Cloud runs the steps in order, stopping at the first failure. Each Machine checks that its step is in a trusted-approved bundle. Attack path. Cloud cannot add, reorder or swap a step after approval, because each step's digest is in the approved bundle; an agent cannot run a staged step before the card is approved.

**Depends on.** B2 and Appendix B gap 7 (the `ProposalApproval` statement).

**Files.**

- [ ] Create `core/crates/ployz-core/src/authority/proposal.rs` with `ProposalApproval`, `StepDigest`, `RecoveryAllowance` and `proposal::verify`.
- [ ] Edit `core/crates/ployz-core/src/authority/restart.rs` so `RestartApproval` gains `proposal: Option<ProposalRef { operation, index }>`.
- [ ] Create `dashboard/src/modules/proposals/tables.ts` (`operation_proposals`), `proposals.server.ts`, `proposals.ts` and `proposals.functions.ts`.
- [ ] Edit `dashboard/src/modules/agent/agent-tools.ts` to add `proposal_create`, `proposal_present` and `proposal_status`, and to route a `GATED` tool call carrying `proposal: <id>` to the draft instead of running it.
- [ ] Edit `dashboard/src/modules/approvals/apply.server.ts` so `applyApproved` runs a proposal's steps in order from its bundle.
- [ ] Create `dashboard/src/components/agent-sidebar/proposal-card.tsx` from the prototype.
- [ ] Edit `dashboard/src/components/agent-sidebar/agent-panel.tsx` to list pending proposals under "Waiting on you".
- [ ] Edit `docs/user/agent.md` (or the page that documents the Agent sidebar) to describe proposals.

**Build.**

- [ ] `ProposalApproval` in `proposal.rs` is a `Statement` under `Purpose::Workflow` with `cluster`, `environment`, `operation`, `steps: Vec<StepDigest { index, kind, digest }>`, `recovery: RecoveryAllowance { retry_failed_step: bool }`, `issued_at`, `expires_at` and `signer`. `proposal::verify` checks that `steps` is ordered and dense and that `expires_at - issued_at` is at most `MAX_LIFETIME_MILLIS`.
- [ ] A step's own statement (for example `RestartApproval`) carries `proposal: Some(ProposalRef)`. `verify_signers` then accepts it without its own signature when the bundle it names is trusted-signed and `SHA-256(canonical(body)) == steps[index].digest`.
- [ ] `operation_proposals` holds `id`, `organizationId`, `environmentId`, `threadId`, `steps` (jsonb of tool, body, digest), `status` (`draft`, `presented`, `approved`, `running`, `done`, `failed`, `denied`), `bundle` (jsonb of the signed statement and grants) and `usedAt`.
- [ ] `proposal_create` returns a draft id. A `GATED` tool call with `proposal: <id>` appends a step and returns `staged` with the step index. `proposal_present` moves the draft to `presented` and returns the card's id. `proposal_status` returns the status and, after a run, each step's result.
- [ ] The card is `Card size="sm"`, `state="destructive"` only when a step destroys data, titled "Run <n> steps in <project>/<environment>", with an ordered list, "If a step fails, the ones after it don't run.", and Approve and Deny. Approve calls `ensureApproval(scope, "proposal")`, which always prompts once, then the browser key approves the bundle.
- [ ] `applyApproved` runs steps in order through the existing `applyService` and `applyVolume` paths, each carrying its `ProposalRef`. The first failure ends the run, records the index, and later steps stay unrun.
- [ ] The remote MCP tool result for a staged step says "Staged as step <n> of proposal <id>. Call proposal_present when the plan is complete; a person approves it in the dashboard."

**You see.**

- [ ] The Agent sidebar shows the card with the ordered steps and one Approve.
- [ ] After "Use passkey" the card becomes `Approved. 3 steps go ahead in shop/production, in order.` and the Machine log shows each step with its proposal index.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `proposal.rs` tests gain a bundle whose step 2 digest was replaced after approval, refused with `Mismatch { field: "steps" }`. Run `cargo nextest run -p ployz-core authority::`.
- [ ] `proposal.rs` tests gain a step statement naming index 5 in a 3-step bundle, refused.
- [ ] `proposal.rs` tests gain a step whose body differs from its digest by one field, refused.
- [ ] `dashboard/src/modules/proposals/proposals.server.test.ts` asserts a `GATED` call with a `proposal` id runs nothing and appends a step. Run `pnpm vitest run src/modules/proposals src/modules/agent`.
- [ ] `proposals.server.test.ts` asserts `applyApproved` stops after a failing step and leaves the rest `unrun`.
- [ ] `dashboard/src/components/agent-sidebar/proposal-card.test.tsx` asserts the card lists steps in order and Approve calls `ensureApproval` once.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5 effort=medium` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Ask the agent to restart `web` at trunk and head with enforcement off. Save `b3-single-gated.png`. Pass when both heads show the existing Approval card and the restart runs.
- [ ] Lane 2. Through the remote MCP, call `proposal_create`, stage `service_restart web`, `service_restart worker` and a settings change, then `proposal_present`. Save `b3-card.png`. Pass when the card lists the three steps in that order with one Approve.
- [ ] Lane 3. Press Approve, use the passkey. Save `b3-approved.png`. Pass when one dialog showed, the card reads approved, and the Machine log shows three steps with indexes 0, 1 and 2.
- [ ] Lane 4. Update the `operation_proposals.steps` row with `psql` to swap steps 0 and 1 after approval, then let the run start. Save `b3-swapped.png`. Pass when the Machine refuses step 0 and the card shows the run failed at step 1 with nothing after it run.
- [ ] Lane 5. Call a `GATED` tool with a `proposal` id and read `proposal_status`. Save `b3-staged-not-run.png`. Pass when the status is `draft` and nothing ran on the Machine.
- [ ] Lane 6. Stage a step that fails (restart of a Service that does not exist) as step 1 of 3. Approve. Save `b3-stop-at-failure.png`. Pass when step 0 ran, step 1 failed, step 2 reads `unrun`.
- [ ] Lane 7. Press Deny. Save `b3-denied.png`. Pass when the card reads `Denied.` and `proposal_status` says denied.
- [ ] Lane 8. Approve a proposal, then present the same draft again. Save `b3-replay.png`. Pass when Cloud answers that the proposal was used and no Machine runs anything.
- [ ] Lane 9. Stage a step that destroys a Volume. Save `b3-destroys.png`. Pass when the card uses the destructive state and badge and names the loss.
- [ ] Lane 10. Dark mode of lane 2. Save `b3-dark.png`. Pass when the card matches the Approval card's tokens.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Time from Approve to the last step's completion for three restart steps at head, against three single approvals at trunk.
- [ ] Probe. Machine log timestamps for the three restarts, five runs each side.
- [ ] Baseline. Record the trunk sum of three single approvals first.
- [ ] Rule. Head within 1.2 times trunk, and the three steps run in order with no overlap.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2, 3 and 9 screenshots into `docs/agents/plans/security-0.4/media/b3-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of staging, the card and the one-prompt approval on a lane VM. Save it as `docs/agents/plans/security-0.4/media/b3-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root appends B3 to the base-branch stack and the operator lands it bottom-up.

## Keep MCP and the CLI on the right side of the key (B4)

Outcome. The remote MCP at `/mcp` and `ployz mcp` both reach gated work only through proposals or the existing Approval card. Neither path ever holds a browser key, a Device key or a grant. A direct CLI command from a trusted Device still runs with no prompt. Attack path. An agent with a Cloud credential cannot produce an approval; a `ployz mcp` process started on a Device-holding laptop cannot borrow the Device key to approve what the agent asked for.

**Depends on.** B3.

**Files.**

- [ ] Edit `core/crates/ployz/src/handlers/mcp.rs` so `ployz mcp` runs tools through the Cloud credential only and refuses to load the Device key, with a startup log line that says so.
- [ ] Edit `dashboard/src/modules/agent/remote-mcp.server.ts` so an `approval_required` result for a staged step carries the proposal id and the card URL, and so a tool call without a `proposal` id on a gated verb returns the existing single Approval card.
- [ ] Edit `dashboard/src/modules/agent/agent-runner.server.ts` so `runGated` never reads `operation_proposals.bundle`.
- [ ] Edit `docs/user/agent/mcp.md` (or the MCP page) to state what MCP can and cannot do, and `docs/user/cli.md` to state that Device commands never prompt.
- [ ] Edit `.agents/skills/verify-mcp-login/SKILL.md` if `ployz mcp login` output changes.

**Build.**

- [ ] `ployz mcp` in `mcp.rs` constructs its Cloud client without the Device keystore. A `PLOYZ_DEVICE_KEY`-style override does not exist. The first stderr line is `ployz mcp: tools run through your Cloud credential; approvals happen in the dashboard`.
- [ ] `remote-mcp.server.ts` returns `{ approval_required: { approval_id, proposal_id, message, url } }` and never an envelope, a grant or a key.
- [ ] The MCP tool descriptions for gated verbs say that a person approves in the dashboard and that `proposal_create` batches several steps under one approval.
- [ ] `ployz restart`, `ployz stop` and `ployz deploy` from a Full or Operator Device keep their behavior from PR #3926 and PR #3939 and show no prompt.

**You see.**

- [ ] `ployz mcp` prints the stderr line above on start, and a gated tool call answers with the card URL.
- [ ] `ployz restart web` from a Device completes with no prompt and the Machine log names the Device.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `core/crates/ployz/src/handlers/mcp.rs` tests assert the MCP client builder has no Device keystore path and that a tool call never serialises a `Signed<_>`. Run `cargo nextest run -p ployz mcp::`.
- [ ] `dashboard/src/modules/agent/remote-mcp.server.test.ts` asserts a gated call answers `approval_required` with `proposal_id` and no `bundle`, `grant` or `signature` key anywhere in the JSON. Run `pnpm vitest run src/modules/agent`.
- [ ] `agent-runner.server.test.ts` asserts `runGated` on an approved proposal reads only its status.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5 effort=medium` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Run `ployz mcp login` then one read tool at trunk and head per `verify-mcp-login`. Save `b4-login.png`. Pass when both heads log in and answer the read tool.
- [ ] Lane 2. From `ployz mcp`, call `service_restart web` with enforcement on. Save `b4-mcp-gated.png`. Pass when the answer carries the card URL and the Machine ran nothing.
- [ ] Lane 3. Start `ployz mcp` on a Device-holding checkout (`verify-cluster cli --device` environment) and inspect its process environment and open files. Save `b4-no-device-key.png`. Pass when the Device keystore is never opened (`strace -e openat` or `lsof` shows no keystore path).
- [ ] Lane 4. Through the remote MCP with an OAuth token, stage and present a proposal, then approve it in the dashboard. Save `b4-remote-proposal.png`. Pass when the agent's `proposal_status` turns `done` and the tool result never held a grant.
- [ ] Lane 5. Capture the remote MCP response bodies with `network har` for lane 4. Save `b4-har.png`. Pass when no body holds `grant`, `signature` or `browserKey`.
- [ ] Lane 6. `ployz restart web` from a Full Device with enforcement on. Save `b4-cli-no-prompt.png`. Pass when no prompt shows and the Machine log names the Device code.
- [ ] Lane 7. `ployz restart web` from an Operator Device with enforcement on. Save `b4-operator-cli.png`. Pass when it runs with no prompt, per PR #3926's Operator rule.
- [ ] Lane 8. Revoke the Organization Token `ployz mcp` uses, call a tool. Save `b4-revoked.png`. Pass when the tool answers unauthorised and the Machine ran nothing.
- [ ] Lane 9. Call `proposal_present` on a draft that belongs to another thread. Save `b4-other-thread.png`. Pass when Cloud refuses and no card appears.
- [ ] Lane 10. Read `docs/user` pages changed by this PR against the live behavior in lanes 2 and 6. Save `b4-docs.png`. Pass when every sentence about MCP and the CLI matches what the lanes saw.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. `ployz mcp` time to first tool answer for a read tool, at trunk and head.
- [ ] Probe. `time` around a scripted stdio session that lists Servers, ten runs each side, interleaved.
- [ ] Baseline. Record the trunk median first.
- [ ] Rule. Head within 100 ms of trunk.

**Review gate.** The operator reviews before merge.

- [ ] Copy lane 2 and 4 screenshots into `docs/agents/plans/security-0.4/media/b4-review-<slug>.png`.
- [ ] Record a 30 to 60 second video of an agent staging a proposal and the person approving it on a lane VM. Save it as `docs/agents/plans/security-0.4/media/b4-review.mp4`.
- [ ] Post the screenshots and the video in chat. Stop at merge-ready. Wait for the operator's click.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root appends B4 to the base-branch stack and the operator lands it bottom-up.

## Carry deploys and settings under the same session (B5)

Outcome. The bottom-bar Deploy and a Service settings change ride the same 15 minute session as restart, so a person deploying three times in a row prompts once. Attack path. Cloud cannot deploy a Service spec the person did not approve, because the envelope's digest covers the Deploy's Applied State delta.

**Depends on.** B2, and thread C's deploy statement (Appendix B gap 6). Until C publishes it, B5 stays unstarted and the root lists it as a blocker.

**Files.**

- [ ] Edit `dashboard/src/modules/approval-session/approval-session.ts` to add `approveDeploy` and `approveSettings`, each producing the statement C defines.
- [ ] Edit the Deploy bottom bar under `dashboard/src/routes/**/$environmentSlug/-components/canvas/**` to call `ensureApproval(scope, "deploy")` before `deploy`.
- [ ] Edit `dashboard/src/modules/config-store/store-write.ts` so a settings commit that reaches Machines calls `ensureApproval(scope, "settings")`.
- [ ] Edit `core/crates/ployz-sdk/src/agent.rs` so the deploy and ensure calls take the approval.
- [ ] Edit `docs/user/deploy.md` to describe the one prompt per 15 minutes.

**Build.**

- [ ] `approveDeploy` signs C's deploy statement with the browser key and attaches the grant. The statement's digest is the Deploy's delta, so a changed spec is a different approval.
- [ ] `approveSettings` does the same for a settings commit that changes what Machines run.
- [ ] A settings edit that never reaches a Machine (a name, a description) asks for nothing.
- [ ] The badge and the dialog copy name "deploys and settings changes" already, from B2.

**You see.**

- [ ] Pressing Deploy inside a live session shows no dialog and the Deployment page opens as today; outside one it prompts first.
- [ ] The Machine log for the Deploy names the passkey and browser key codes.

**Verify, unit.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] `approval-session.test.ts` asserts `approveDeploy` for delta A does not verify for delta B (digest mismatch) against the Rust fixture. Run `pnpm vitest run src/modules/approval-session`.
- [ ] C's statement tests gain a deploy under a grant whose families lack `deploy`, refused with `Mismatch { field: "families" }`. Run `cargo nextest run -p ployz-core authority::`.
- [ ] `store-write.test.ts` asserts a description edit calls `ensureApproval` zero times.

**Verify, live.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked. Ten lanes on `claudeAgent/claude-opus-5-5 effort=medium` at the PR head, per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Deploy `production` with enforcement off at trunk and head. Save `b5-enforce-off.png`. Pass when both heads deploy with no dialog.
- [ ] Lane 2. Enforcement on, no session, press Deploy. Save `b5-first-deploy.png`. Pass when the dialog shows before the Deploy row exists.
- [ ] Lane 3. Deploy three times inside one session. Save `b5-three-deploys.png`. Pass when one dialog showed and three Deployments completed.
- [ ] Lane 4. Change `replicas` on `web` inside a session. Save `b5-settings.png`. Pass when no dialog shows and the Machine log names the passkey for the settings change.
- [ ] Lane 5. Change `web`'s description. Save `b5-description.png`. Pass when no dialog shows and no envelope is sent (`network requests` shows no approval field).
- [ ] Lane 6. Edit the Deploy's delta in `psql` between approval and run. Save `b5-tampered-delta.png`. Pass when the Machine refuses and the Deployment page shows the refusal.
- [ ] Lane 7. Deploy from `fix-api` inside a `production` session. Save `b5-scope.png`. Pass when the `scope` dialog shows.
- [ ] Lane 8. Deploy from `ployz deploy` on a Device. Save `b5-cli.png`. Pass when no prompt shows and the log names the Device.
- [ ] Lane 9. Restart `web` right after a deploy in the same session. Save `b5-mixed.png`. Pass when no second dialog shows.
- [ ] Lane 10. Dark mode of lane 2. Save `b5-dark.png`. Pass when the dialog uses the tokens in `styles.css`.

**Verify, perf.** Tests alone are not sufficient verification. A PR is verified only when its unit, live, and perf boxes are all checked.

- [ ] Metric. Time from the Deploy press to the Deployment page's first row, at trunk and at head inside a session.
- [ ] Probe. `agent-browser` click with `performance.now()` stamps through `eval`, ten runs each side, interleaved.
- [ ] Baseline. Record the trunk median first.
- [ ] Rule. Head within 200 ms of trunk.

**Review gate.** None. B5 is not review-gated.

**Merge.**

- [ ] Root's clean verdict at the exact head SHA.
- [ ] Bugbot triage done.
- [ ] Rebased onto current trunk after the verdict, patch-id unchanged.
- [ ] The root appends B5 to the base-branch stack and the operator lands it bottom-up.

## Close the program

- [ ] Every box above is checked with its evidence.
- [ ] Reply to the operator with the report the execution playbook names.

## Appendix A. Data shapes

The chain has three links. Each is a canonical JSON body with a signature over `ployz-authority/v1\n<purpose>\n<body>`, as PR #3926 defines, so one `Signed<T>` type carries all of them.

**Trusted key.** `TrustedKey` in `core/crates/ployz-core/src/authority/trusted_key.rs` (B0, A owns the contract). Fields are `kind` (`Device { key: DeviceKey }` or `Passkey { credential_id, public_key: CosePublicKey }`), `authority` (`Manage` for a Device, `ApproveWorkflows` for a passkey), `code` (first 10 base32 characters of the SHA-256 of the public key, the same rule as `DeviceCode::of`) and `added_by: DeviceKey`. A Machine stores its trusted keys in its own record next to the `trusted_machines` PR #3939 pinned, and reads only that list when it verifies. The Cloud `device_request` and `passkey_request` tables are relays and prove nothing. A passkey is not a Device. It cannot open a management connection and it usually holds a P-256 key, so it never gets a `device-<code>` Management Client slot. `ployz device ls` lists it under the Devices with `approve-only`.

**Passkey enrollment.** `PasskeyEnrollment` in `trusted_key.rs` (B0) is a `Statement` under `Purpose::Enforce` with `cluster: SealingPublicKey`, `key: CosePublicKey`, `credential_id`, `authority: ApproveWorkflows`, `account` (the email, for `device ls`), `issued_at`, `expires_at` and `signer: DeviceKey`. Only a Full Device signs it. A Machine that verifies it adds the trusted key.

**Session grant.** `SessionGrant` in `core/crates/ployz-core/src/authority/session.rs` (B1) and `dashboard/src/modules/approval-session/approval-session.ts`. Its `body` is canonical JSON of `browser_key` (base64url raw Ed25519 public key), `cluster` (Sealing public key), `environment` (the namespace), `machines` (sorted distinct Machine ids), `families` (a subset of `start`, `stop`, `restart`, `deploy`, `settings`), `issued_at`, `expires_at` (at most `validity::MAX_LIFETIME_MILLIS` later) and `delegation: "none"`. Its `assertion` is a `PasskeyAssertion` with `credential_id`, `authenticator_data`, `client_data_json` and `signature`, all base64url. The WebAuthn challenge is `SHA-256(body)`, so the passkey's answer covers exactly that body. The prototype's grant body (with no `cluster` or `machines` yet, since the sketch has no Cluster identity in the browser) reads `{"browser_key":"...","delegation":"none","environment":"<id>","expires_at":...,"families":[...],"issued_at":...}`.

**Browser key.** A non-extractable WebCrypto Ed25519 key pair made by `crypto.subtle.generateKey({ name: "Ed25519" })`. HeadlessChrome 151 in the prototype made one. The private half lives in page memory; `sessionStorage` holds the public fields for the badge. B2 moves the `CryptoKey` object into IndexedDB (non-extractable keys can be stored there) so a reload keeps the session; closing the tab still ends it. Its code is the first 10 base32 characters of the SHA-256 of the public key, shown on hover as `key <code>`.

**Action envelope.** A `Signed<RestartApproval>` (B1) whose `signatures` holds one `KeySignature { key: browser key, signature }` and whose new `grants: Vec<SessionGrant>` holds the grant. `RestartApproval` keeps every field from PR #3926 and its `signer` becomes `Signer::{Device(DeviceKey), Browser(SigningPublicKey)}`. The Machine's `verify_signers` accepts a signature from a key it does not trust only when a grant names that key as `browser_key`, the grant's assertion verifies against a trusted `ApproveWorkflows` key, the grant's `cluster`, `environment`, `machines` and `families` cover the statement, and the statement's `expires_at` is at most the grant's. Then `restart::verify` and `admit_operator_restart` run unchanged, including the consumption log at `/var/lib/ployz/restart-approvals.json`. Deploy and settings statements (B5) come from thread C and ride the same `Signed<T>` with the same grant rule.

**Proposal bundle.** `ProposalApproval` in `core/crates/ployz-core/src/authority/proposal.rs` (B3) is a `Statement` under `Purpose::Workflow` with `cluster`, `environment`, `operation: OperationId`, `steps: Vec<StepDigest { index, kind, digest }>` (dense, ordered, `digest` is the SHA-256 of the step's canonical body), `recovery: RecoveryAllowance { retry_failed_step: bool }`, `issued_at`, `expires_at` and `signer`. It is signed once, by the browser key under a grant or by a Device. Each step's own statement carries `proposal: Some(ProposalRef { operation, index })` and no signature of its own. A Machine admits a step when the bundle it names is trusted-signed, the step's digest matches, and the bundle's window covers now. Cloud stores the bundle in `operation_proposals.bundle` and runs the steps in order through `applyApproved`; a changed, added or reordered step fails its digest on the Machine.

**How a Machine verifies each link against its own keys.** Every check reads the Machine's trusted-key list and nothing from Cloud. An envelope's signature is checked against the browser key named in the envelope; the browser key is checked against the grant; the grant's assertion is checked against a trusted passkey; a proposal step is checked against its bundle, and the bundle against a browser key under a grant or a Device. Refusals reuse `AuthorityRefusal` (`Mismatch { field }`, `Expired`, `NoClusterIdentity`) so Cloud and the CLI show one vocabulary.

**TypeScript.** `@ployz/sdk/authority` (B1, C1 gap 1) exports the ts-rs output of `authority` (`Signed`, `KeySignature`, `RestartApproval`, `SessionGrant`, `PasskeyAssertion`, `ProposalApproval`). The dashboard's `approval-session.ts` builds those shapes and never a type of its own. `canonicalJson` there follows `ployz_core::authority::canonical` and its tests reuse the Rust fixtures.

## Appendix B. C1 gaps

What B needs from A's contract that PR #3926 and PR #3939 do not provide. "B inside authority" means a B owner can add the code under `core/crates/ployz-core/src/authority/` with the A owner's review; "waits" means A or C must change a shape first.

1. No TypeScript export of authority types. Neither PR touches `core/crates/ployz-sdk`, `build-cloud-sdk.sh` or `native-dts.mjs`, so `Signed`, `RestartApproval` and `KeySignature` never reach `@ployz/sdk`. B1 adds the export. B inside authority and the SDK build; no shape change.
2. No approval parameter on the Cloud SDK calls. `ployz-sdk/src/agent.rs` `stop_container` and `start_container` (lines 147 and 175) and the `EnsureContainer` path take no approval, so Cloud has no way to forward an envelope. B1 adds `approval: Option<Signed<RestartApproval>>`. B inside the SDK; A reviews the daemon side.
3. No session grant or passkey assertion type and no WebAuthn verification. `Purpose` has `Workflow`, `Join` and `Enforce`; nothing verifies `clientDataJSON`, `authenticatorData` or a COSE key. B1 adds `session.rs`. The COSE and P-256 dependency choice (`p256` plus a small COSE parser, or `webauthn-rs-core`) is A's call under bet 9. Waits for that call, then B inside authority.
4. `Signed<T>` carries no grants and `#[ts(concrete(T = RestartApproval))]` pins its TypeScript type. B1 adds `grants: Vec<SessionGrant>` and lifts the pin. Waits for A, since it changes every existing statement's wire shape.
5. No trusted-key record beyond `DeviceKey`. Today a Device is a Full-scope Management Client slot, `device_request.key` is checked as 64 hex characters and `DeviceCode::of` hashes an Ed25519 key. A passkey fits none of that. B0 adds `TrustedKey`, `KeyAuthority` and `PasskeyEnrollment`. Waits for A's agreement on the record and on where the Machine stores it.
6. No deploy or settings statement. PR #3926 covers restart and stop only. B5 waits for thread C's statement and its `RestartFacts` equivalent.
7. No proposal bundle statement. B3 adds `proposal.rs` and the `proposal` field on step statements. B inside authority after gap 4 lands.
8. Enforcement is Full-only and CLI-only. `set_device_enforcement` refuses everything but a Full Device, so a dashboard-only customer cannot turn it on. B does not change that; the first Device for such a customer comes over SSH plus a short code, as `next-steps.md` records. Recorded here so no B owner adds a Cloud switch.

## Appendix C. Multi-Cluster

An Organization can hold several Clusters. Approvals stay inside one Cluster because every statement names `cluster: SealingPublicKey` and `machines`. A session grant names one Cluster and one Environment; an Environment's Namespace lives on one Cluster, so "per Environment" already implies "per Cluster". A press in an Environment on another Cluster is out of scope and prompts again. The passkey is enrolled per Cluster, by a Full Device of that Cluster, and a Cluster that never enrolled it refuses its grants with `Mismatch { field: "signatures" }`. Cloud stores one Sealing public key per Organization today (`dashboard/src/modules/machines/tables.ts:543`); B reads the Cluster from the Environment's Machines, not from that column, and thread C's per-Cluster column replaces it. No approval, grant or proposal is ever reused across Clusters, and no statement lists Machines from two Clusters.

## Appendix D. Open decision, session length and action families

**Decided 2026-11-07 by Nick: 60 minutes per Environment, not 15.** The session grant gets its own 60-minute cap. Each statement the browser key signs keeps `validity::MAX_LIFETIME_MILLIS` (15 minutes), so a grant can outlive any one approval but no approval outlives its grant. Update B1's grant type, B2's copy ("next 60 minutes") and the expiry tests to match. The text below is the earlier recommendation, kept for its reasoning.

Recommendation. Confirm the plan's choice. One passkey prompt opens 15 minutes for one Environment and the families `start`, `stop`, `restart`, `deploy` and `settings`. Volume deletion, removing a Service that holds a Volume, pruning and any other change that loses data asks every time, one change per prompt, with the loss named in the dialog. An agent proposal asks once per proposal even inside a live session, because the step list is the agent's text and the prompt binds the person to it.

Why 15 minutes. It equals `validity::MAX_LIFETIME_MILLIS`, so the grant needs no new core constant and no statement can outlive its grant. It covers a normal deploy-check-restart loop (the prototype's deploy took under two minutes). A longer window would need a core change and widen what a stolen page can do.

Why per Environment. The Environment is the unit a person thinks in and the unit the Protected flag already guards. A per-Cluster session would prompt less and let a `fix-api` session touch `production`, which is the blast radius the thread exists to shrink. A per-Service session would prompt on every drawer, which the prototype showed is one prompt too many for a restart loop.

Why these families. They are the actions that change what a Machine runs without losing data, and the only ones Cloud drives today. Settings edits that never reach a Machine (a name, a description) ask for nothing.

Alternatives weighed and rejected. A 60 minute window (exceeds the core lifetime cap and the time a laptop sits unlocked). A session per Organization (crosses Clusters). A prompt per press (the restart loop becomes three prompts in two minutes; the prototype's second press with no prompt is the experience to keep). A session that survives tab close (the key would have to be extractable or stored for days).

## Appendix E. Residual risks

- A script running in the dashboard page (XSS, a bad dependency) can use the in-memory browser key for the rest of the window, inside its scope. The scope and the 15 minute cap bound it. The dashboard's content security policy is thread F's; B1's review names it.
- The Machine must know the Cloud origin to check `clientDataJSON.origin`. Self-hosted Cloud sets it at enrollment (`PasskeyEnrollment` carries the allowed origin). A wrong origin refuses every grant, which fails safe and is loud.
- Machine clocks. `CLOCK_SKEW_MILLIS` (2 minutes) applies to grants as to approvals. A Machine two minutes slow refuses a fresh grant; the drawer shows the refusal and the person retries.
- Platform passkeys are mostly ES256. Machines gain a P-256 verifier (bet 9, lean on proven primitives). The prototype's virtual authenticator chose EdDSA; a real iPhone will not.
- Losing every passkey leaves the SSH Device path, as today. Nothing in B weakens it.
- Cloud can delay or drop a press, as today. It cannot change one, because the envelope covers the request id, the Service and the kind.
- A replay inside the window is refused by the consumption log for restart and stop (PR #3926). Deploy and settings need their own operation ids from thread C before B5 lands.
- `ployz mcp` on a laptop that also holds a Device key never loads that key (B4). A person who pastes a Device key into an agent's environment defeats this; docs say not to.

## Appendix F. Prototype evidence

Branch `t3/sec04-b-sketch` in the worktree `/home/codex/.t3/worktrees/ployz-rust/t3-sec04-b-sketch`, from `origin/main` at `a2e7b24a5e`, uncommitted and never pushed. It ran under `SERVERS=1 scripts/verify/up.sh` with one real Machine, and `agent-browser` drove it with a CDP virtual authenticator (`.audit/b-sketch/virtual-authenticator.mjs`). Screenshots are in `.audit/b-sketch/` of the main worktree. The sketch adds `dashboard/src/modules/approval-session/`, `dashboard/src/components/agent-sidebar/proposal-card.tsx`, a mounted `ApprovalPrompt` in the Organization route, an optional `approval` field on `actServiceServerFn`, and a session badge in the Service drawer header.

Proved.

- A real `navigator.credentials.create` and `navigator.credentials.get` run in HeadlessChrome 151 with a CDP virtual authenticator; the passkey chose EdDSA (`alg -8`). The first Restart press opens "Confirm it's you" before any request leaves the page (`01-first-press-prompt.png`).
- "Use passkey" makes a non-extractable WebCrypto Ed25519 browser key, a canonical grant body with the challenge `SHA-256(body)`, and an envelope for the press. The server function logged the envelope's body, browser key and grant body, and the restart ran on the Machine (`02-after-passkey-restarting.png`, `docker ps` showed `web` up 19 seconds beside containers up 7 minutes).
- The badge reads `Approved for production · 15 min left` beside the drawer's actions and never says "sign".
- A second Restart press inside the window shows no dialog and still logs a fresh envelope (`03-later-press-no-prompt.png`, badge `14 min left`).
- After the session expires, the badge turns to `Approval ended` and the next press opens "Confirm it's you again" (`04-expired.png`).
- Switching to `fix-api` in place keeps the production session; its badge reads `Approved for production only` and a Restart there opens "This needs its own approval" (`05-out-of-scope.png`).
- A staged proposal shows in the Agent sidebar under "Waiting on you" as `Run 3 steps in shop/production` with its ordered steps (`06-proposal-card.png`). Approve opens one "Approve the agent's steps" dialog (`07a-proposal-prompt.png`) and ends in `Approved. 3 steps go ahead in shop/production, in order.` (`07-proposal-approved.png`).

Unproven.

- No Machine verified anything. The daemon ran with enforcement off and the envelope stopped at Cloud's log. B1 proves the chain on the Machine.
- The grant has no `cluster` or `machines` yet, because the browser has no Cluster identity to name. B1 reads it from the Environment's Machines.
- The proposal card is fixture-driven (`localStorage`), not a Cloud row. B3 builds the row and the MCP tools.
- Chrome on a laptop shows its own passkey sheet over the dialog; the prototype's virtual authenticator shows none. B1's review video records the real sheet.
