# Servers (SRV)

Adding Servers (pasted line, SSH, standalone, Fleet Tokens and the waiting list), approving joins, devices, passkeys and CI or agent keys, roles and regions, draining, upgrading by hand and on a Release Channel, removing and forgetting, the recovery code, and what a Server does on its own (Offline detection, address changes, the WireGuard mesh, firewall upkeep, Global catch-up, keeping a forgotten Server out). The install script, cloud-init, the uninstaller and self-hosted Cloud are in [install.md](install.md); `ployz server logs` is in [observe.md](observe.md); `ployz server volumes` (kept anonymous volumes) is in [services.md](services.md). Verifiers start with `scripts/verify-cluster up --machines 2` (Standalone) or `--cloud` for anything that names Cloud, the waiting list or the dashboard; `--bare` for joins from a pasted line. Compat facts used below: the 0.3.19 CLI has no `key`, `region`, `device pin` or `server upgrade-policy` commands, no `server add --key/--region/--cluster-name/--ingress-closed`, no `device approve --join/--this-device` and no `server enroll --region/--yes`; a 0.3.x Server can't join a 0.4 Cluster (`sealed Sealing Key is malformed`, #4194); Release Channels stay on 0.3.x, so no 0.3 Server reaches 0.4 by channel.

### SRV-01 Add a server by pasting one line
- **Surface:** CLI
- **Reach:** `ployz server add --command [--name web-1] [--storage zfs|none] [--label-add K=V] [--accepts-ingress false]` on your computer, signed in; paste the printed line on the Server and the `pmet_…` token at the hidden prompt (or `export PLOYZ_ENROLL_TOKEN=pmet_…` before the line; or `--token-stdin`). `--json` returns line and token at once, then `ployz server add --wait <ENROLLMENT>`
- **Proof:** the line prints the chosen name (`MY-SERVER` → `my-server`), then `Waiting for the Server's daemon to start…`; the waiting command exits 0 and `ployz server ls` lists the Server `up`; a two-line paste at the prompt is refused and nothing is kept; the printed line carries `--name`, `--storage`, `--label-add` and `--key pk1_…`
- **Needs:** 1–2 Machines (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud, Server↔Server: a 0.3.19 CLI's line carries no `--key`; a 0.3.x Server joining a 0.4 Cluster fails with `sealed Sealing Key is malformed`
- **Docs:** servers/add-a-server.md#add-one-server-that-joins-right-away

### SRV-02 Add a server over SSH from an approved device
- **Surface:** CLI
- **Reach:** `ployz server add root@HOST [--name web-2] [--storage zfs] [--version <v>] [--no-install] [--public-ip auto|none|IP] [--wg-endpoint IP[:PORT]]`
- **Proof:** prints `Joined Server web-2.`; `ployz server ls` lists web-2 `up` on the version of the `ployz` you ran; the CLI signs the join with your device key, so no join code is printed; from a computer with no device key it stops with `web-2 can't join from here: this computer has no device key… Nothing was enrolled.`
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud (or Standalone through a context)
- **Drive:** verify-server (`verify-cluster add` enrolls this way)
- **Compat:** CLI→daemon, Server↔Server: a 0.3.x joiner can't join a 0.4 Cluster
- **Docs:** servers/add-a-server.md#join-through-ployz-cloud-without-an-approved-device

### SRV-03 Put a new server in a region
- **Surface:** CLI
- **Reach:** `ployz server add --command --region fsn` (or `root@HOST --region fsn`); also MCP `server_add` (returns the `ployz server add … --region` line and adds nothing)
- **Proof:** `ployz server ls` REGION column reads `fsn` (`FSN` lowers to `fsn`); `--region fsn --label-add region=lhr` stops before anything starts; rerunning for a joined Server with another region stops and names the Server's region; `server_add` returns a command and `ployz server ls` is unchanged
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server; verify-region-placement for what later Deploys do
- **Compat:** CLI→Cloud: 0.3.19 has no `--region` (use `--label-add region=`)
- **Docs:** servers/add-a-server.md#put-the-server-in-a-region

### SRV-04 Approve a join from the CLI with its join code
- **Surface:** CLI
- **Reach:** on the computer whose device the pasted line carries: `ployz device approve --join <20-char code>`; then rerun the join line on the Server or click **Enroll** again
- **Proof:** before signing, the CLI shows name, subnet, endpoints and the devices and passkeys the Server will trust; after the rerun `ployz server ls` lists the Server `up`; from another computer it stops with `Server web-2 trusts only passkey …, not this computer's device …. Nothing was approved.`; an approval older than 15 minutes is refused for a new admission
- **Needs:** 2 Machines (`--bare` for the joiner) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server; verify-approval-binding
- **Compat:** CLI→Cloud, Server↔Server: 0.3.19 `device approve` has no `--join`
- **Docs:** servers/add-a-server.md#approve-a-join

### SRV-05 Approve a join with a passkey in the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers` → **Waiting for approval** row → **Approve with passkey** → passkey prompt; then **Enroll** the Server again
- **Proof:** the row shows the join code; after the passkey answer and Enroll the Server leaves the tray and lists online; for a passkey that belongs to someone else the row says so and shows no button
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud · External: a WebAuthn authenticator (agent-browser virtual authenticator if available)
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server: a 0.3.x Server has no passkey trust and can't join a 0.4 Cluster
- **Docs:** servers/add-a-server.md#approve-a-join

### SRV-06 Refuse to sign a join relayed through an unpinned Cloud
- **Surface:** CLI
- **Reach:** signed in, no pin for the Organization: `ployz server add root@HOST` or `ployz device approve --join CODE` reaching the Cluster through Cloud
- **Proof:** stops with `Server web-2 would join a Cluster this computer hasn't pinned, and the join was relayed through Ployz Cloud, which can't vouch for it… Nothing was approved.`; after `ployz device pin --connect ssh://root@HOST` the same command signs
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud: 0.3.19 has no pin and no such refusal
- **Docs:** servers/add-a-server.md#approve-a-join

### SRV-07 Trust the key on an install line, and replace it only at the Server's terminal
- **Surface:** CLI
- **Reach:** on the Server: `sudo ployz key add pk1_…`; a second install line with a different `--key` on a Server that already has a first key
- **Proof:** `/var/lib/ployz-first-key.json` holds the key; a different key asks for the new key's code at the terminal and never takes it from `--yes` or the environment; with no terminal (or `--json`) it changes nothing and prints `sudo ployz key add pk1_…`; after `ployz-uninstall` the file reads `"state":"spent"`
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** install line→CLI: a 0.3.x `ployz` doesn't know `--key`
- **Docs:** servers/add-a-server.md#the-key-on-an-install-line

### SRV-08 Start a standalone Cluster without Cloud
- **Surface:** CLI
- **Reach:** on the Server `sudo ployz server add --standalone --context NAME`, or `ployz server add --standalone root@HOST --context NAME`; a second Server with `ployz server add --standalone root@HOST2 --context NAME`
- **Proof:** `ployz ctx` shows NAME as current; `ployz server ls --context NAME` lists the Server(s) `up`; run on the Server it prints `ployz device approve --this-device --connect ssh://root@HOST` and approves no device; the SSH form approves this computer's device (`ployz device ls`)
- **Needs:** 1–2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`verify-cluster up` without `--cloud`)
- **Compat:** CLI→daemon: `--standalone` is a hidden flag (absent from `--help`)
- **Docs:** servers/add-a-server.md#add-a-standalone-server

### SRV-09 Approve a device on the first server
- **Surface:** CLI
- **Reach:** `ployz device approve --this-device --connect ssh://root@HOST` (one `--connect` per Server); or `ployz device request` on your computer, then on the Server `sudo ployz login` and `sudo ployz device approve CODE`
- **Proof:** `ployz device ls` lists this computer's device; before approval `ployz deploy` of a Service with a Volume is refused naming `ployz.container.signed-version.v1`, after it succeeds; run on the Server's own terminal, `--this-device` refuses
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→daemon, upgrade: 0.3.19 has no `--this-device`; a 0.3 Server upgraded to 0.4 by exact version enforces at once and needs this before its first signed deploy (PLOY-12)
- **Docs:** servers/add-a-server.md#your-first-servers-first-key

### SRV-10 Get an install command for the first server from the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers` with no Server → **Get install command** (also the environment bottom bar's **Add servers**)
- **Proof:** the card shows the install line with the token under it and **Shown once. Expires in 24h.**, then **Listening for servers…** until a Server checks in; with no passkey the line carries no `--key`; for a member the button says only owners or admins can
- **Needs:** 0–1 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 to see a Server arrive)
- **Compat:** Cloud→install line: the line pins the release Cloud's SDK speaks
- **Docs:** servers/add-a-server.md#add-servers-from-the-dashboard

### SRV-11 Name the Cluster and start it behind Cloudflare Tunnel
- **Surface:** dashboard
- **Reach:** first-run card → **Use Cloudflare Tunnel instead of public ports** → **Cluster name**, Cloudflare API token, **Find zones**, **Check** → **Get install command**; CLI: `ployz server add --command --cluster-name acme --ingress-closed`
- **Proof:** card reads **Ready for example.com**; after the join the Servers page says **Cloudflare Tunnel is set up on example.com** and `docker ps` on the Server shows no `ingress-holder` on 80/443; with the switch off the card says **Without Cloudflare Tunnel, your servers open ports 80 and 443.**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: Cloudflare account and zone
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** CLI→Cloud, Server↔Server: a Server whose `ployz` predates recorded doors can't join a closed or tunnel Cluster; 0.3.19 has no `--cluster-name`/`--ingress-closed`
- **Docs:** servers/add-a-server.md#choose-a-name-and-cloudflare-tunnel-first

### SRV-12 Make, list and revoke Fleet Tokens
- **Surface:** CLI
- **Reach:** `ployz server token create [--name rack-b] [--expires 1d|7d|30d] [--auto --max N] [--json]`; `ployz server token ls`; `ployz server token revoke <name>`; also dashboard `/cloud/ada/~/servers/tokens` (**Revoke**)
- **Proof:** create prints the `pflt_…` token once and `curl -fsSL https://ployz.sh | sudo sh -s -- <cli version> --enroll`; `--json` has `fleet_token.token` and `install`; `token ls` and the Tokens page list waiting/joined counts, expiry, last use (**Auto up to N**, **x/N joined**); revoke drops waiting Servers at once and the toast counts them; joined Servers stay; an Organization Token is refused
- **Needs:** 1 Machine (`--bare`) to wait on the token · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard for the Tokens page
- **Compat:** CLI→Cloud: 0.3.19 has the same `token` commands
- **Docs:** servers/add-a-server.md#add-many-servers-with-a-fleet-token (`token ls` only in prose and --help)

### SRV-13 Add servers dialog with Shell and cloud-init lines
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers` → **Add servers** (dialog "New fleet token") → name, expiry → **Create** → **Shell** / **cloud-init** tabs; see verify-dashboard features/organization.md
- **Proof:** Shell tab shows `curl -fsSL https://ployz.sh | sudo [PLOYZ_CLOUD_URL='…'] sh -s -- <version> --enroll --key pk1_…` and the token once; cloud-init tab carries `PLOYZ_FLEET_TOKEN=pflt_…` in `runcmd`; with no trusted passkey the dialog shows no line and names who can add Servers plus `sudo ployz key add pk1_…`
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** servers/add-many-servers.md#prepare-your-servers

### SRV-14 Review servers waiting to join
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers` → **Waiting to join** tray; click a row for the detail sheet; **Reject** in the sheet or header
- **Proof:** rows show name, OS, source IP, provider, CPU, memory, disk and a status of **Installing Docker**, **Ready**, **Building ZFS**, **Blocked** (with the Server's own reason, e.g. `Disk is 20 GiB. Ployz needs 30 GiB.` or `UDP port 51820 is in use by …`) or **Offline**; Blocked rows can't be selected; over ten rows a filter and search appear; Reject removes the row and the Server stops checking in
- **Needs:** 1–3 Machines (`--bare`, installed with a Fleet Token) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 plus verify-server `--bare`
- **Compat:** check-in→Cloud: enrollment wire needs `ployz` ≥ 0.3.0-beta.6 (`ENROLL_MIN_CLI_VERSION`); older shows **Update ployz**
- **Docs:** servers/add-a-server.md#add-servers-from-the-dashboard

### SRV-15 Enroll waiting servers with the Enroll sheet
- **Surface:** dashboard
- **Reach:** select rows → **Enroll** → **Region** (pick or **New region…**), **First server**, **Volumes**, **Names** (Hostnames or Pattern `web-{n}`), **Services/Web traffic/Builds** → **Enroll N**
- **Proof:** Servers move to **Enrolling**, then list online with names `web-1`, `web-2`; a duplicate hostname gets `-2`; a region name whose key exists is refused with **Region Falkenstein (fsn) already exists.**; with no Server reachable for 3h the sheet says **Servers offline**
- **Needs:** 2–3 Machines (`--bare`) · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 plus verify-server
- **Compat:** Cloud→Server: a 0.3.x Server can't join a 0.4 Cluster
- **Docs:** servers/add-many-servers.md#enroll-the-selection

### SRV-16 Follow, cancel and retry enrollments
- **Surface:** dashboard
- **Reach:** **Enrolling** list rows → **Cancel**; open a failed Server
- **Proof:** rows read **Queued**, **Installing**, **Waiting on first**, **In line** with position, six progress segments; Cancel after Cloud sent the join refuses with `web-1 may already be in the Cluster: Cloud sent its join. Nothing cancelled.`; a cancelled Server returns to the bottom of Waiting with **Last enroll cancelled**; **Joined** rows stay one hour; at most eight join at a time after the first
- **Needs:** 3+ Machines (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 plus verify-server
- **Compat:** none
- **Docs:** servers/add-many-servers.md#follow-progress

### SRV-17 Enroll servers on arrival
- **Surface:** dashboard
- **Reach:** **Add servers** → **Enroll on arrival**, **Maximum servers** N; CLI `ployz server token create --auto --max 3 --expires 7d`
- **Proof:** with an existing Cluster, a Server installed with the token joins on its next check-in with its hostname and all three roles; on a spare disk it gets a ZFS pool on that disk; the Tokens page reads **x/N joined**; Server N+1 stays in Waiting; in a new Organization the install ends `No cluster yet. Enroll one server in Ployz Cloud to start it.`
- **Needs:** 2–3 Machines (`--bare`) · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard
- **Compat:** check-in→Cloud
- **Docs:** servers/add-many-servers.md#enroll-on-arrival

### SRV-18 List and inspect waiting servers from the CLI
- **Surface:** CLI
- **Reach:** `ployz server ls --waiting [--json]`; `ployz server show <name|id prefix ≥8>`
- **Proof:** rows show id start, name, provider, IP, status, age; `server show` on a waiting Server shows CPU, memory, disks, Docker, the storage it would get and blockers; two matches list their ids instead
- **Needs:** 1–2 Machines (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud: same in 0.3.19
- **Docs:** servers/manage-servers.md#servers-waiting-to-join

### SRV-19 Enroll waiting servers from the CLI
- **Surface:** CLI
- **Reach:** `ployz server enroll 'host-*' --name 'web-{n}' [--region fsn] [--storage zfs|none] [--no-spare-disk] [--no-fallback] [--first [NAME]] [--all] [--status] [--yes] [--json]`; `ployz server enroll --status` resumes following
- **Proof:** prints the plan (names, founder, storage, source IPs, skips) and asks; `--json` without `--yes` returns `confirmation_required`; a skip exits 3 with reasons; `--status` prints stage lines (**Queued**, **In line · 3 servers ahead**)
- **Needs:** 2–3 Machines (`--bare`) · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud: 0.3.19 `server enroll` has no `--region` or `--yes`
- **Docs:** servers/manage-servers.md#servers-waiting-to-join

### SRV-20 Reject waiting or enrolling servers from the CLI
- **Surface:** CLI
- **Reach:** `ployz server reject 'web-*' [--yes] [--json]`, or by name or id prefix
- **Proof:** lists matches with ids, asks, prints `2 rejected. They won't come back under the same Fleet Token.`; outside a terminal without `--yes` fails with `confirmation_required`; an enrolling Server whose join Cloud sent prints `web-1 may already be in the Cluster: Cloud sent its join`; `--json` lists `enrolling` and `joining`
- **Needs:** 2 Machines (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud
- **Docs:** servers/manage-servers.md#servers-waiting-to-join

### SRV-21 See every server and its status
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers`; CLI `ployz server ls [--json]`
- **Proof:** a row per Server with services (three names then **+N**, or **No services**), region (**Default** until set) and a status word (**Online**, **Building**, **Offline**, **Unknown**, **Upgrading**, **Upgrade failed**, **Docker only**); CPU and memory meters only with metrics on (External: ClickHouse); **Fleet** counts online/building/offline/unknown and upgrades due; `server ls` shows REGION and status `up`/`down`; a Server that never joined reads down with no version and names `ployz server forget <id> --confirm dead`
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud (CLI also Standalone)
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server: 0.3 Servers show no metric history in main Cloud (PLOY-12)
- **Docs:** servers/manage-servers.md#check-your-servers

### SRV-22 See what runs on a server
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers/<serverId>` → **Resources**, **Running here**
- **Proof:** CPU, memory and disk meters, charts over **1h**–**30d**, disk bar split into **Volumes**, **Mirrors**, **Logs and metrics**, **Other**, **Free**; Running here lists each Service with its CPU and memory; offline, each reads **Still on** another Server or **Down**; an older Server reads **Upgrade web-1 to see its metrics**. Without ClickHouse, Resources reads **Metrics aren’t enabled on this Server** with a **Set up metrics** link, and **Ployz → Version** still shows the daemon version, equal to `daemon_version` in `ployz --json server ls`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse (`--profile observe` or `OBSERVE_CLICKHOUSE_URL`) for meters and charts
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: 0.3 Servers show no history
- **Docs:** servers/manage-servers.md#see-what-runs-on-a-server

### SRV-23 Show one server's telemetry, round trips and last upgrade
- **Surface:** CLI
- **Reach:** `ployz server show web-1 [--json]`; also MCP `server_show`
- **Proof:** prints OS, versions, addresses, storage, CPU, memory, disk, `peer = web-2 <ms>` round trips and the latest upgrade attempt; `--json` includes daemon features; a peer with no handshake reads `peer = web-3 down, no WireGuard handshake for 6m`; `server_show` returns state, release and newer one, Environments and leftovers
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon
- **Docs:** servers/manage-servers.md#servers-waiting-to-join

### SRV-24 Members are refused Server access through Cloud
- **Surface:** CLI
- **Reach:** as a member: `ployz server ls`, `ployz exec …`, `ployz server set …` through Cloud
- **Proof:** exits 1 with `Server access needs an admin. Ask an owner or admin of acme.`; `ployz logs` still works, capped at 200 lines per Server; an admin demoted to member loses access within 5 minutes
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--cloud` (second signed-in member CLI)
- **Compat:** CLI→Cloud
- **Docs:** servers/manage-servers.md#reach-a-server-from-the-cli

### SRV-25 Request and approve a device
- **Surface:** CLI
- **Reach:** `ployz device request` (prints a 20-char code); from an approved device `ployz device approve <code> [--yes]`
- **Proof:** approve shows Full access and asks; afterwards `ployz device ls` lists the device and `ployz exec <service> -- true` works from it; a 10-char code from an older CLI is refused; a key from Cloud that doesn't hash to the code is refused; a Server that was down didn't get the device (rerun approves it)
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server; verify-approval-binding
- **Compat:** CLI→daemon, CLI→Cloud: Servers on 0.3.5+ refuse a ≤0.3.4 CLI's `exec` through Cloud with `needs Full access`
- **Docs:** servers/manage-servers.md#approve-a-device

### SRV-26 List and remove devices and passkeys for good
- **Surface:** CLI
- **Reach:** `ployz device ls [--json]`; `ployz device rm <code>`
- **Proof:** `device ls` shows this device and passkeys by full code (`passkey, approve-only`), others by 10 chars; `device rm` lists what each Server removed or `not present`, exits 3 with failures or omissions; an `exec` the removed device started stops before `device rm` returns; a re-approval of that key is refused; the last trusted key is refused; removing this computer's device warns and names `device.key`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) or local Cloud
- **Drive:** verify-server; verify-approval-binding
- **Compat:** CLI→daemon: 0.3 Servers don't keep the full removed key (`not marked removed` on retry)
- **Docs:** servers/manage-servers.md#approve-a-device

### SRV-27 Pin your Cluster's key before sealing through Cloud
- **Surface:** CLI
- **Reach:** `ployz device pin --connect ssh://root@HOST` (or `sudo ployz device pin` on a Server)
- **Proof:** before the pin `ployz set --secret …` through Cloud refuses and names `ployz device pin`; after it the secret seals; a plain TCP or Cloud-chosen connection is refused; a Server whose key differs from Cloud's is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud: absent in 0.3.19
- **Docs:** servers/manage-servers.md#pin-your-clusters-key

### SRV-28 Add a passkey and make servers trust it
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/account` → **Add passkey**; then on each Server `sudo ployz key add pk1_…`
- **Proof:** **Account** lists which Servers trust the passkey; `key add` names the Servers that still don't; `ployz device ls` shows it as `passkey, approve-only` with who approved it (`on <server>`); a passkey can't deploy or exec
- **Needs:** 1–2 Machines · Volumes: none · Cloud: local Cloud · External: a WebAuthn authenticator
- **Drive:** verify-dashboard SERVERS=1; verify-server
- **Compat:** Cloud→Server: 0.3 Servers have no passkeys
- **Docs:** servers/manage-servers.md#add-a-passkey

### SRV-29 Make a CI or agent key
- **Surface:** CLI
- **Reach:** `ployz key add --ci --env production --name github-actions [--service web] [--volume db/data] [--project P]` or `--agent --env staging`; `ployz key ls [--json]`
- **Proof:** shows the code, environments, volumes and Servers and asks; prints `PLOYZ_KEY=...` once; `key ls` lists kind, name, scope, volumes and trusting Servers; `PLOYZ_KEY=… ployz deploy` ships a new image of an approved Service; `--agent` with no `--env` is refused; with no terminal it refuses
- **Needs:** 2 Machines · Volumes: Managed (ZFS) for `--volume` · Cloud: no (Standalone) or local Cloud
- **Drive:** verify-server; verify-approval-binding
- **Compat:** CLI→daemon: absent in 0.3.19; silent (CI) deploys need `parent-versions.v1` on every Server, so a mixed cluster with a 0.3 Server refuses them
- **Docs:** servers/manage-servers.md#add-a-ci-or-agent-key

### SRV-30 Narrow or widen a key's scope
- **Surface:** CLI
- **Reach:** `ployz key scope <code> --env production --service web [--volume …] [--name …] [--yes]`
- **Proof:** prints current and new grant per Server; a narrowing goes through without asking; a widening, a dropped volume or name, or a silent Server asks, and `--yes` makes it fail instead; a non-answering Server exits 3
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: absent in 0.3.19
- **Docs:** servers/manage-servers.md#add-a-ci-or-agent-key

### SRV-31 Rotate a CI or agent key
- **Surface:** CLI
- **Reach:** `ployz key rotate <code> [--events FILE]`
- **Proof:** lists Services running a version the old key signed (image, signed at, digest), asks, prints a new `PLOYZ_KEY=...`, redeploys each and follows to the end; `ployz key ls` then shows only the new key; a deploy with the old key mid-rotation is caught (after three looks it keeps the old key and exits 3); a rerun after interruption reuses the same new key
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-approval-binding
- **Compat:** CLI→daemon: absent in 0.3.19
- **Docs:** servers/manage-servers.md#add-a-ci-or-agent-key

### SRV-32 Remove a leaked CI or agent key
- **Surface:** CLI
- **Reach:** `ployz key rm <code>`
- **Proof:** the key leaves `ployz key ls`; a container it approved keeps running, but after `docker stop` its Server refuses to start it with `The key that approved this was rotated or removed; deploy again.`; a device or passkey code is refused (use `device rm`)
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-service-floor
- **Compat:** CLI→daemon: absent in 0.3.19
- **Docs:** servers/manage-servers.md#add-a-ci-or-agent-key

### SRV-33 Change a server's name, labels, roles, public IP or build concurrency
- **Surface:** CLI
- **Reach:** `ployz server set web-1 [--name web-a] [--label-add K=V] [--label-rm K] [--accepts-builds B] [--accepts-services B] [--build-concurrency N|auto] [--public-ip IP|none]`; also MCP `server_set` (builds, concurrency, services, web traffic off; turning web traffic off on the last Server asks a human)
- **Proof:** `ployz server ls --json` shows the new values; the command waits until the entry Server lists the change (an immediate `ployz deploy` plans from it) and warns when it doesn't in time; `server_set` can't turn web traffic on
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon
- **Docs:** servers/manage-servers.md#change-what-a-server-does

### SRV-34 Stop new services starting on a server
- **Surface:** dashboard
- **Reach:** server page → **Services** → **Run services here** off
- **Proof:** after reload the switch stays off; a deploy places no new replica there; if no other Server runs services, deploys fail until it's back on; while a drain runs, turning it on is refused
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server
- **Docs:** servers/manage-servers.md#change-what-a-server-does

### SRV-35 Turn web traffic off or on from the server page
- **Surface:** dashboard
- **Reach:** server page → **Web traffic** → **Takes web traffic** → **Stop** / switch on → **Retry** on failure
- **Proof:** reads **Your domains point at <IP>**, then **Stopping…** and **Web traffic is off**; turning on reads **Starting the web server…** and `docker ps` shows `ingress-holder` (80/443) and `ingress`; with port 80 taken it reads **Couldn't start web traffic · Port 80 is in use on web-1. Free it, then try again.** and the switch goes back off
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** upgrade: after 0.3→0.4 the Ingress Proxy stays on `local` logging until it's turned on again
- **Docs:** servers/manage-servers.md#services-with-a-volume

### SRV-36 Turn web traffic on or off from the CLI
- **Surface:** CLI
- **Reach:** `ployz server set web-1 --accepts-ingress=true|false [--yes]`
- **Proof:** `=true` starts `ingress-holder` and `ingress`; with port 80 held it prints `Web traffic stays off on web-1: port 80 or 443 is in use…` and exits 3; another failure prints `Server updated; the Ingress Proxy did not follow` with `next:`/`inspect:`/`retry:` lines and exits 3; `=false` keeps Caddy running and, on the last ingress Server, asks (non-interactive needs `--yes`)
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-caddy-load
- **Compat:** CLI→daemon, upgrade: the json-file logging switch for the Ingress Proxy happens here after 0.3→0.4
- **Docs:** servers/manage-servers.md#services-with-a-volume

### SRV-37 Turn builds on or off and set builds at once
- **Surface:** dashboard
- **Reach:** server page → **Builds** → **Run builds here**, **Builds at once**; CLI `ployz server set web-1 --accepts-builds=false --build-concurrency 2`
- **Proof:** after reload the switch and combobox hold; `ployz --json server ls` shows `servers[].machine.accepts_builds: false` and `build_concurrency: 2`; with a second Server, the next build runs there
- **Needs:** 1 Machine (2 for the next-build check) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 (SERVERS=2 for the next-build check)
- **Compat:** Cloud→Server: Cloud writes the Server's policy; a 0.3.19 Server takes the same `accepts_builds` field
- **Docs:** builds/where-builds-run.md#build-on-your-servers

### SRV-38 Drain a server from the dashboard
- **Surface:** dashboard
- **Reach:** server page → **Services** → **Drain** → dialog → confirm
- **Proof:** dialog badges **Moves with data**, **Skipped**, **Stays**, **Global**; the Drain row counts **Pending** then what moved, stopped, stayed, failed; `ployz ps` shows the replicas on other Servers and the new containers served before the old ones stopped (ack probe/curl loop sees no gap); a second drain reads **Queued · Waiting on web-1**; a Server that doesn't answer gives `web-2 isn't answering, so Cloud can't tell what draining it would stop…`; incomplete drains offer **Drain again**
- **Needs:** 2–3 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2; verify-drain-copy
- **Compat:** Cloud→Server: Cloud refuses drains onto old Servers by capability (`Drains need a newer Ployz on web-1`)
- **Docs:** servers/manage-servers.md#change-what-a-server-does

### SRV-39 Drain a server from the CLI
- **Surface:** CLI
- **Reach:** `ployz server drain web-2 [--json] [--context|--connect …]`; also MCP `server_drain`
- **Proof:** exits 0 when everything moved, non-zero if anything stayed or failed; `ployz server ls --json` shows services off on web-2; signed out with a copy that needs a signature: `error: Nothing drained from web-1: shop-production/api needs a device's signature… next: ployz login`; a global Service stops it with `…runs \`shop-production/metrics\` on every Server…`; a stopped drain ends `Services are now off on web-2: turn them back on…`
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud (also Standalone with `--context`)
- **Drive:** verify-server; verify-drain-copy
- **Compat:** CLI→daemon: a drain of a 0.2.4 Server turns services off then stops with `It cannot serve ployz.container.ensure.v1.`
- **Docs:** servers/manage-servers.md#change-what-a-server-does

### SRV-40 A drain moves a service's volume with it
- **Surface:** dashboard
- **Reach:** drain a Server that holds a single-container Service's Managed Volume (dialog or `ployz server drain`)
- **Proof:** Drain row reads **Draining · Moving data to web-3**; `ployz volume runs data` lists the run; afterwards the Service runs on web-3 with the same rows (md5 of a file via `ployz exec`); a Service with two volumes or shared writes stays with its reason; a failed move names the release run
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server with the ack probe; verify-drain-copy, verify-volume-runs
- **Compat:** Cloud→Server: volume routes changed (`cut.running.v1`, `start.v1` …); an older drained Server reads **Upgrade web-1 to move data with the drain**
- **Docs:** servers/manage-servers.md#services-with-a-volume

### SRV-41 Set a server's region from its page
- **Surface:** dashboard
- **Reach:** server page → **Placement** → **Region** → pick, **No region**, or **New region…** → **Move server** in the **Move web-2 to Sydney** dialog
- **Proof:** the field reads the new region and **Queued** until the Server reports it; the dialog lists each affected Service with **→ web-3** or **Waits for a server in Falkenstein**; nothing restarts (`ployz ps` container ids unchanged); a key that exists is refused; members see it read-only
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2; verify-region-placement
- **Compat:** Cloud→Server: Cloud sets only the `region` label
- **Docs:** servers/manage-servers.md#set-a-servers-region

### SRV-42 Set a server's region from the CLI
- **Surface:** CLI
- **Reach:** `ployz server set web-1 --label-add region=fsn` (`--label-rm region` clears it)
- **Proof:** `ployz server ls` REGION column reads `fsn` (`-` when cleared); no container restarts; the Servers page shows it on next refresh
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: works the same from 0.3.19
- **Docs:** servers/manage-servers.md#set-a-servers-region

### SRV-43 List and rename regions
- **Surface:** CLI
- **Reach:** `ployz region ls [--json]`; `ployz region rename default Falkenstein`; also MCP `region_list`, `region_rename`
- **Proof:** `region ls` prints REGION, KEY, SERVERS and Servers with no region below with the `ployz server set` fix; rename prints `Renamed default to Falkenstein. Nothing restarts.` and the Servers list shows **Falkenstein**; a 41-character or duplicate name is refused
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--cloud`
- **Compat:** CLI→Cloud: absent in 0.3.19
- **Docs:** servers/manage-servers.md#list-and-rename-regions (`region ls` is documented here although the coverage scan missed it)

### SRV-44 Rename a region in the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers` → **Rename region** → pick region, type name → **Rename region**
- **Proof:** dialog shows the CLI key (`--region default`); the list and server pages show the new name at once and after reload; duplicate or over-long names show an error under the field and keep the text; disabled for members
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** servers/manage-servers.md#list-and-rename-regions

### SRV-45 Upgrade every server that's behind from the Servers page
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers` → **Upgrade to `<version>`**
- **Proof:** top reads **Upgrading 1/N**, each row **Upgrading** in name order; afterwards each server page shows the new release and `ployz server ls --json` agrees; curl to an app keeps answering (brief 503 allowed)
- **Needs:** 2 Machines on an older release (`verify-cluster up --daemon <older>`) · Volumes: none · Cloud: local Cloud · External: a channel pointer newer than the Servers (`PLOYZ_CHANNEL_URL` test channel)
- **Drive:** verify-dashboard SERVERS=2 plus verify-server; verify-upgrade-attempt
- **Compat:** upgrade: channels keep 0.3.x on 0.3.x, so a 0.3.19 Cluster is never offered 0.4 here
- **Docs:** servers/manage-servers.md#upgrade-ployz-on-a-server

### SRV-46 Upgrade servers automatically on the Release Channel
- **Surface:** daemon
- **Reach:** **Upgrade settings** icon → **Upgrade automatically** on (or `ployz server upgrade-policy set --automatic true`)
- **Proof:** upgrading starts at once, then hourly; only releases out ≥3 days are taken; an offline Server reads **Upgrades to `<v>` when it's back**; a failing upgrade rolls that Server back, it reads **Upgrade failed**, the rest wait, and each member gets one email (MAIL=1)
- **Needs:** 2 Machines on an older release · Volumes: none · Cloud: local Cloud · External: a channel release ≥3 days old (test channel)
- **Drive:** verify-server; verify-dashboard MAIL=1; verify-upgrade-attempt
- **Compat:** upgrade: no 0.3 Server reaches 0.4 by channel
- **Docs:** servers/manage-servers.md#turn-on-automatic-upgrades

### SRV-47 Upgrade one server from its page
- **Surface:** dashboard
- **Reach:** server page → **Ployz** section reading **`<v>` is out** → **Upgrade**; after a failure **Show details**, **Try again**
- **Proof:** the section shows the new release after reload; **Show details** has the stage and error to copy; only one upgrade runs per Organization (a second waits); disabled for members and while building or offline
- **Needs:** 1 Machine on an older release · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1; verify-upgrade-attempt
- **Compat:** upgrade
- **Docs:** servers/manage-servers.md#upgrade-one-server

### SRV-48 Choose Stable or Beta releases
- **Surface:** dashboard
- **Reach:** **Upgrade settings** → **Releases** Stable/Beta; CLI `ployz server upgrade-policy show` and `ployz server upgrade-policy set --channel beta`, both published by Cloud (signed in; without the cached list, `show` is refused as an unrecognized subcommand)
- **Proof:** the Servers page and server pages show the newest release on that channel; switching back to Stable never downgrades (a beta ahead reads current); `ployz server upgrade-policy show` prints both settings
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** CLI→Cloud: a Cloud-published command, so a 0.3.19 CLI that reads the published manifest gets it from main Cloud too
- **Docs:** servers/manage-servers.md#choose-stable-or-beta-releases

### SRV-49 Upgrade servers from the CLI through Cloud
- **Surface:** CLI
- **Reach:** `ployz server upgrade stable web-1 web-2 [--json]`; also MCP `server_upgrade` (one Server, Organization channel)
- **Proof:** one line per Server; `--json` `attempts[].server`, `unattempted`; the other channel fails with `channel_mismatch`; an offline or building Server is skipped, exit 3, and the retry command printed; a Server on the newest says `already runs <v>`; Ctrl-C leaves the upgrade running and `ployz server show` reports the end
- **Needs:** 2 Machines on an older release · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server; verify-upgrade-attempt
- **Compat:** CLI→Cloud, upgrade
- **Docs:** servers/manage-servers.md#upgrade-from-the-cli

### SRV-50 Upgrade or downgrade to an exact version over SSH
- **Surface:** CLI
- **Reach:** `ployz server upgrade 0.4.0-beta.2 web-1 --connect ssh://root@HOST [--allow-downgrade] [--json]` (or `--context`)
- **Proof:** prints `Downgraded Server machine-1 from … to ….` or `already runs …`; `--json` has `from_version` and `change` (`upgraded|downgraded|unchanged`); a downgrade asks, and under `--json` refuses without `--allow-downgrade`; `9.9.9` fails `No Ployz release 9.9.9.` (`not_found`); from Cloud or a non-approved CLI the Server refuses with `ployz.machine.upgrade.exact.v1 needs Full access`; a powered-off Server mid-upgrade exits 3 after 5 minutes with `stopped answering during upgrade`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server ("Roll one Server", `power off`); verify-upgrade-attempt
- **Compat:** CLI→daemon, upgrade: the 0.3.19 → 0.4 path is by exact version only, enforces at once, and every Service needs one signed deploy after (PLOY-12; no docs page describes it)
- **Docs:** servers/manage-servers.md#upgrade-from-the-cli

### SRV-51 Refuse an unsigned release and use test download mirrors
- **Surface:** daemon
- **Reach:** `ployz server upgrade 0.3.0-beta.2 web-1 --connect …`; set `PLOYZ_GITHUB_URL`, `PLOYZ_FALLBACK_URL`, `PLOYZ_CHANNEL_URL` in `/etc/default/ployz`, `sudo systemctl restart ployz`, then `ployz server upgrade stable web-1`
- **Proof:** the unsigned release is refused before install; a release whose checksums aren't signed by the Ployz key or claim another version is refused; with the mirror vars, the Server downloads from `<base>/releases/download/v<version>/…` and follows the test channel's `v0/stable`; an `http://` or credentialed URL is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone) · External: a mirror of release assets (draft-release copy)
- **Drive:** verify-server
- **Compat:** upgrade
- **Docs:** servers/manage-servers.md#upgrade-from-the-cli

### SRV-52 Features name the server that needs an upgrade
- **Surface:** CLI
- **Reach:** in a mixed Cluster with one older Server: `ployz logs api`, `ployz exec`, `ployz volume move`, a dashboard Drain or log page
- **Proof:** `ployz logs` prints other Servers' lines plus `! Server web-2 needs a newer Ployz. It cannot serve ployz.logs.tail.v1.` and exits 3; `exec` exits 1 with `…cannot serve ployz.container.exec.v1.`; the logs page shows **web-2 needs an upgrade to show its logs**; a volume move reads `Volume move needs a newer Ployz on web-1…`
- **Needs:** 2 Machines on mixed releases (`--daemon machine-2=<older>`) · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard SERVERS=2
- **Compat:** CLI→daemon, Cloud→Server: the mixed 0.3.19/0.4 cell (M3)
- **Docs:** servers/manage-servers.md#servers-on-different-versions

### SRV-53 Upgrade a server too old for the relay over SSH or on the server
- **Surface:** CLI
- **Reach:** `ployz server upgrade stable web-1 --connect ssh://root@HOST`; on the Server `sudo ployzd install --version stable --software-only`
- **Proof:** through Cloud/relay the CLI stops with `This Server runs Ployz 0.2 or a 0.3.0 beta up to beta.6, which this release can't reach through the relay.` and the Servers page reads **Upgrade your server over SSH**; after the SSH upgrade `ployz server ls` reports the new version and the Server keeps its apps and volumes; a 0.3.2 Server prints `…can't show which Cluster it belongs to…`
- **Needs:** 1–2 Machines on 0.2.4 or 0.3.0-beta.6 (cached release) · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** upgrade, CLI→daemon
- **Docs:** servers/manage-servers.md#servers-on-02-or-030-beta6-and-earlier

### SRV-54 Remove a server from the dashboard
- **Surface:** dashboard
- **Reach:** server page → **Danger** → **Remove server** → type name → **Remove**
- **Proof:** dialog lists each Volume only that Server holds as `pg-data` in `shop/production` or **Anonymous volume**; while Services run it shows their count next to **Drain**; after removal the Server leaves the list and its containers keep running untouched; for the last Server the toast says it still runs its containers and names `sudo ployz-uninstall`; an old intermediate Server stops it with `Server removal needs a newer Ployz on web-1`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2; verify-member-removal
- **Compat:** Cloud→Server
- **Docs:** servers/manage-servers.md#remove-a-server

### SRV-55 Remove a server from the CLI
- **Surface:** CLI
- **Reach:** `ployz server rm web-2 [--confirm web-2] [--accept-volume-loss <name>] [--json]`; MCP `server_remove` is SRV-76
- **Proof:** warns which Services a drain could still move and prints the drain command; after removal warns of Services with no replicas and prints `ployz deploy`; the Server is never reached and keeps running its containers, and for the last Server `still_running` is true; `--json` lists `under_replicated`, `no_copies`, `cloud_kept`; outside a terminal without `--confirm` fails `confirmation_required` naming the volumes only that Server holds; a Server that doesn't answer needs `--confirm dead`; only from an approved device or over SSH; through Cloud it asks yes/no first; `ployz ps` meanwhile warns `machine-2 was removed while this ran; its rows are missing.` and exits 3
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone) and local Cloud
- **Drive:** verify-server; verify-member-removal
- **Compat:** CLI→daemon, CLI→Cloud: removal through a Server older than 0.3.4 names the Server's ID instead
- **Docs:** servers/manage-servers.md#remove-a-server

### SRV-56 Forget an offline server from the dashboard
- **Surface:** dashboard
- **Reach:** offline server page → **Danger** → **Forget web-2** → **Forget server** → type name → **Forget**
- **Proof:** the dialog lists Volumes whose only copy is there; after confirm the Server leaves the list within seconds; the button appears only while the Server shows Offline; for the last Server the row offers Forget Servers instead
- **Needs:** 2 Machines (one `power off`) · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2 plus verify-server `power off`; verify-member-removal
- **Compat:** Cloud→Server
- **Docs:** servers/manage-servers.md#forget-a-server-that-is-gone

### SRV-57 Forget a dead server from the CLI
- **Surface:** CLI
- **Reach:** `ployz server forget web-2 --confirm dead [--json]`; also MCP `server_forget`
- **Proof:** prints `Forgot Server web-2.`; offline takes seconds, up-but-silent about 12 minutes with a progress line each minute; a Server that answers is forgotten too, its sole-copy Volumes confirmed with `--accept-volume-loss`; `--confirm not-this` on a silent Server asks for `dead`
- **Needs:** 2 Machines (one powered off, or management ports dropped per verify-server "Running but unreachable to forget") · Volumes: Managed (ZFS) · Cloud: local Cloud and Standalone
- **Drive:** verify-server; verify-member-removal
- **Compat:** CLI→Cloud, Server↔Server: a forgotten Server on 0.3.9 or earlier can put itself back on the list
- **Docs:** servers/manage-servers.md#forget-a-server-that-is-gone

### SRV-58 A forgotten server stays out
- **Surface:** daemon
- **Reach:** forget a running Server cut off by `partition on`, then `partition off`; or rerun `ployz server add` for it
- **Proof:** it keeps running its containers; the other Servers refuse it as member or WireGuard peer; a plain re-add is refused and the token can't add it again
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone) or local Cloud
- **Drive:** verify-server (`partition on/off`); verify-member-removal
- **Compat:** Server↔Server: 0.3.17+ for a Server forgotten mid-join
- **Docs:** servers/manage-servers.md#forget-a-server-that-is-gone

### SRV-59 Add a removed or forgotten server back with a reset
- **Surface:** CLI
- **Reach:** on the host `sudo ployz key add pk1_…` (new install command's key), then `ployz server add root@HOST --reset [--yes]` with a new token (or `--command` line)
- **Proof:** asks before resetting; afterwards `ployz server ls` lists it with a new id; a plain add first is refused with the message pointing to `--reset` and a new token; volume copies on it show under the Volume's **Copies** when kept
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-member-removal
- **Compat:** Server↔Server: the re-added Server must run 0.3.10+
- **Docs:** troubleshooting/servers.md#a-forgotten-server-is-refused-when-you-add-it-again

### SRV-60 Forget every server after deleting them at the provider
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings` → **Danger** → **Forget servers** → type the slug (or **Forget them and start over.** on the Servers page); CLI `ployz server forget --confirm <slug>`
- **Proof:** lists Servers and Volumes; refuses while any Server answers; afterwards the Servers page shows the first-run card, earlier `ployz server add` lines stop working (tokens deleted), projects and history stay; the next Server added gets Environments deployed to it; disabled for members
- **Needs:** 1–2 Machines (stopped with `systemctl stop ployz.socket ployz` or powered off) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 plus verify-server
- **Compat:** Cloud→Server
- **Docs:** servers/manage-servers.md#forget-servers-you-deleted

### SRV-61 Reset a founding attempt that never finished
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings` → **Reset founding attempt** → tick **I confirm the old Machine has been stopped or erased.** → **Reset enrollment**
- **Proof:** the Organization page's founding alert clears; the next `ployz server add --command` founds the Cluster again; other tokens keep working; after a completed founding the reset says there's nothing to reset (or points to Forget Servers)
- **Needs:** 1 Machine (stopped mid-founding) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 plus verify-server
- **Compat:** CLI→Cloud
- **Docs:** troubleshooting/servers.md#my-first-server-never-finished-joining

### SRV-62 Reset a founding attempt from the CLI
- **Surface:** CLI
- **Reach:** `ployz cloud reset [--yes] [--json]`
- **Proof:** with an unfinished founding it clears it, and the next `ployz server add` founds again; with a finished founding it says there's no founding to reset
- **Needs:** 1 Machine (stopped mid-founding) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud: same command in 0.3.19
- **Docs:** **undocumented** (only --help; the troubleshooting page names only the dashboard button)

### SRV-63 Clean up leftovers of a deleted environment
- **Surface:** dashboard
- **Reach:** server page → **Running here** → **Namespaces no Project owns** → **Remove** → type name; CLI `ployz server clean` then `ployz server clean --namespace my-app-staging --confirm my-app-staging`; also MCP `server_clean`
- **Proof:** the leftover shows **Not in any Project**; after Remove its containers and volumes are gone from every Server (`docker ps -a`, `zfs list`); the CLI asks yes/no and refuses with `--context` or `--connect`
- **Needs:** 1–2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** CLI→Cloud
- **Docs:** servers/manage-servers.md#clean-up-after-a-deleted-environment

### SRV-64 Get and rotate the recovery code
- **Surface:** CLI
- **Reach:** first `ployz server add` of an Organization in a terminal (or `--json --show-recovery-code`); later `ployz recovery rotate --connect ssh://root@HOST [--show-recovery-code]`
- **Proof:** the founding add prints **Recovery code** once and `Keep the recovery code safe; ployz recover needs it.`; later adds print none; rotate prints a new code and the old one stops opening the backup; with no terminal it refuses and changes nothing; a member is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--cloud`
- **Compat:** CLI→daemon, CLI→Cloud: rotate exists in 0.3.19
- **Docs:** servers/recovery-code.md#get-your-recovery-code

### SRV-65 Recover the Sealing Key on a server that needs recovery
- **Surface:** CLI
- **Reach:** after forgetting every Server and adding one: `sudo ployz recover` on it, or `ployz recover --connect ssh://root@HOST [--code-stdin]`
- **Proof:** before: the add says `This Server needs access to the Sealing Key; ployz recover restores it.`, the Servers page shows **Needs recovery**, `ployz set --secret` says `Your server needs the Sealing Key back…`; a wrong code changes nothing; after: a deploy that opens a sealed value succeeds and `ployz set --secret` works within seconds
- **Needs:** 1–2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--cloud`
- **Compat:** CLI→daemon: `recover` exists in 0.3.19
- **Docs:** servers/recovery-code.md#when-a-server-shows-needs-recovery

### SRV-66 A stopped or powered-off server shows Offline
- **Surface:** daemon
- **Reach:** `sudo systemctl stop ployz.socket ployz` on a Server, or `verify-cluster power off`
- **Proof:** within about ten seconds the Servers page reads **Offline** and the server page **Can't reach web-2**; its containers read **Server offline**, not Crashed; `ployz server ls` from another Server reads `down`; starting Ployz again brings it back online in the same place
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`power off/on`); verify-dashboard SERVERS=2
- **Compat:** Cloud→Server
- **Docs:** troubleshooting/servers.md#a-server-shows-offline

### SRV-67 A server picks up its new address after a restart
- **Surface:** daemon
- **Reach:** change a Server's public IP, restart Ployz on it
- **Proof:** `ployz server ls` shows the new address and peers reach it; an address set with `--public-ip` or `--wg-endpoint` never changes on its own; a failed public IP lookup keeps the old one with a warning in `journalctl -u ployz`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: an IP change (Incus provider, or a Boat resume, which the lab says to avoid)
- **Drive:** verify-server (`--provider incus`)
- **Compat:** upgrade: a Server upgraded from ≤0.3.0-beta.6 treats both addresses as chosen
- **Docs:** troubleshooting/servers.md#a-servers-address-changed

### SRV-68 Tell servers where one is after every IP changed
- **Surface:** CLI
- **Reach:** `ployz server set web-2 --wg-endpoint 203.0.113.20[:51821]`
- **Proof:** prints `web-1 now dials web-2 at 203.0.113.20:51820.` then `Updated Server web-2.`; the two handshake within seconds and `ployz server ls` reads both `up`; if web-2 can't be reached the second line reads `! web-2 didn't answer, so its record may or may not have changed.` and it still succeeds
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: simultaneous IP change (simulate with a wrong `--wg-endpoint` first)
- **Drive:** verify-server
- **Compat:** CLI→daemon
- **Docs:** troubleshooting/servers.md#servers-lost-each-other-after-their-ips-changed

### SRV-69 Servers reach each other over the WireGuard mesh, and commands name a missing handshake
- **Surface:** daemon
- **Reach:** block UDP 51820 between two Servers (`partition on`), run `ployz server ls`, `ployz server show web-1` through web-1
- **Proof:** `server ls` warns `web-1, the Server this command entered by, has had no WireGuard handshake with web-3 for 6m.` and still exits 0; `server show` lists `peer = web-3 down, no WireGuard handshake…`; after `partition off` round trips return; with three Servers web-3 can still read `up` with `! web-3 did not answer; its rows are missing.`
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`partition on/off`)
- **Compat:** Server↔Server: before 0.4.0 these said only that web-3 didn't answer
- **Docs:** troubleshooting/servers.md#the-server-a-command-entered-by-cant-reach-another

### SRV-70 Cloud reaches servers through the relay
- **Surface:** daemon
- **Reach:** a Server with outbound https only (no direct path from Cloud); filter `relay.ployz.dev`
- **Proof:** Cloud lists and commands the Server through `relay.ployz.dev`; with the relay filtered, adding the first Server fails `Cloud can't reach this Server through the relay or directly` and the Servers page reads **Can't reach your servers right now**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: the production relay (lab Machines never reach it; management goes direct)
- **Drive:** none in the lab (verify-server points `relay.ployz.dev` at loopback)
- **Compat:** Cloud→Server: Servers on 0.2 or ≤0.3.0-beta.6 can't be reached through the relay
- **Docs:** troubleshooting/servers.md#cant-reach-your-servers-right-now

### SRV-71 Ployz puts its firewall rules back within a minute
- **Surface:** daemon
- **Reach:** on a Server, `iptables -D DOCKER-USER -i ployz-wg -o ployz -j ACCEPT` (or insert a rule above Ployz's)
- **Proof:** within a minute `journalctl -u ployz` logs `restored firewall rules that were missing or no longer first: filter DOCKER-USER: -i ployz-wg -o ployz -j ACCEPT` and cross-Server calls to services work again with no restart; holding the iptables lock logs `failed to reapply firewall rules`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** upgrade: 0.3.16 and earlier restore rules only on restart
- **Docs:** troubleshooting/servers.md#services-on-other-servers-stop-answering-after-a-firewall-change

### SRV-72 A joining server runs Global catch-up
- **Surface:** daemon
- **Reach:** deploy a Service set to run on every Server, then `ployz server add` a new Server
- **Proof:** after the join, `ployz ps <service>` shows a replica on the new Server; joined with an older `ployz` that can't read a peer, it prints `Joined Server …` then `error: Server joined, but Global catch-up is incomplete; it remains a Cluster member.` with `update ployz on this host`, and the Server stays in `ployz server ls`
- **Needs:** 2–3 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: a joining `ployz` older than its peers can't read their answers (≤0.3.3 vs sealed variables)
- **Docs:** troubleshooting/servers.md#a-new-server-joined-but-global-catch-up-cant-read-another-server

### SRV-73 A server whose cluster store was lost refills it
- **Surface:** daemon
- **Reach:** stop Ployz, delete `/var/lib/ployz/corrosion/store.db`, start Ployz; or `docker stop ployz-corrosion`
- **Proof:** commands through that Server fail with `error: this Server's Cluster store holds no other Server…` until it refills, usually within a minute, then `ployz server ls` through it lists every Server; a stopped `ployz-corrosion` restarts within seconds
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/servers.md#a-servers-cluster-store-was-lost

### SRV-74 Retry a failed add, and see servers that never joined
- **Surface:** CLI
- **Reach:** interrupt `ployz server add root@HOST` after Cloud took the Server (e.g. kill during daemon install), rerun with other `--storage`; or use a token on a host that never joins, then `ployz server add` another
- **Proof:** the retry finishes the same Server with the same address; the next add warns `Server web-3 may still be joining` and `--json` lists it under `unarrived` with `next` = `ployz server forget <id> --confirm dead`; after `server forget` the retry is refused naming `--reset` and a new token
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud
- **Docs:** servers/add-a-server.md#good-to-know

### SRV-75 A server restored from a disk snapshot picks its services back up
- **Surface:** daemon
- **Reach:** snapshot a Server's disk at the provider, change something, restore the snapshot
- **Proof:** after boot `ployz ps` shows its services and `docker ps` its web server as at snapshot time; the next deploy updates them
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone) · External: provider disk snapshots (Incus snapshot may stand in)
- **Drive:** verify-server (`--provider incus`)
- **Compat:** none
- **Docs:** servers/manage-servers.md#see-what-runs-on-a-server

### SRV-76 An agent asks to drain, remove or forget a server
- **Surface:** MCP
- **Reach:** `ployz mcp` (stdio), remote `/mcp` on Cloud, or the dashboard Agent sidebar: call `server_drain`, `server_remove`, `server_forget` on a Server of a claimed Organization, then in an unclaimed agent sandbox
- **Proof:** in a claimed Organization the call is refused and the agent names the person's step (the dashboard dialog or `ployz server rm|forget|drain …`); `ployz server ls` is unchanged; in an unclaimed sandbox the same call runs and `server_show` no longer lists the Server; `server_forget` on a Server that answers and holds a Volume copy is refused
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2 (Agent sidebar needs `ANTHROPIC_API_KEY`) or `ployz mcp` against the `--cloud` run
- **Compat:** none
- **Docs:** account/connect-an-agent.md#approve-a-deploy (docs disagree: servers/manage-servers.md#remove-a-server says an agent can't remove or forget, account/agent-sidebar.md#what-it-can-do-on-your-servers lists **server remove** and **server forget**, and the `server_drain` description says it asks a human only for a global Service; verify which holds)
