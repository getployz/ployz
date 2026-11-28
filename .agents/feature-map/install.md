# Install (INS)

Getting Ployz onto machines: the `ployz` CLI install line, the server install script (`core/install.sh`: version or channel argument, `--enroll`, `--key`, `PLOYZ_FLEET_TOKEN`, `PLOYZ_CLOUD_URL`, `PLOYZ_SKIP_ZFS`, the download mirrors), cloud-init user data, storage choices at install (ZFS on the main disk, a spare disk, Docker only), the host checks the installer and daemon refuse on (Docker, nftables, firewalld, UDP 51820, OS, disk), `ployz-uninstall`, and self-hosted Ployz Cloud (`dashboard/self-host/compose.yml`: configure, start, upgrade, the optional `observe` profile). Joining, enrolling and approving after the install are in [servers.md](servers.md). Verifiers start on `scripts/verify-cluster up --bare` hosts (fresh Ubuntu 24.04 Boat sandboxes, UFW on, nothing installed; `--distro debian-12` for the ZFS build path); a published install line keeps a staged `ployzd` whose version matches (`ployzd <version> retained`). Compat facts used below: `install.sh --key` breaks only when passed with a 0.3 version; `ENROLL_MIN_CLI_VERSION = 0.3.0-beta.6`; Release Channels (`v0/stable`, `v0/beta`) stay on 0.3.x, so `curl … | sh` and `sh -s -- beta` install 0.3.x until promotion; the self-host Cloud database crosses migrations 0047–0059 and a beta.3 → beta.2 rollback likely breaks on 0057.

### INS-01 Install or upgrade the CLI with one line
- **Surface:** install
- **Reach:** `curl -fsSL https://ployz.sh | sh`; `… | sh -s -- beta`; `… | sh -s -- 0.4.0-beta.3`; `curl -fsSL https://ployz.sh | INSTALL_BIN_DIR="$HOME/.local/bin" sh`
- **Proof:** prints `Installed ployz to /usr/local/bin/ployz` and `ployz --version` equals the resolved version; `sh -s -- 9.9.9` ends `There's no Ployz release 9.9.9. Releases: …`; without OpenSSL 3 or minisign it stops `Cannot verify the release signature: install OpenSSL 3 or minisign, then rerun`; a tampered `checksums.txt` ends `Release signature verification failed…`; running it again upgrades in place
- **Needs:** 1 Machine (`--bare`) or this host · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server `--bare`
- **Compat:** upgrade: `stable` and `beta` resolve through `ployz.sh` channel pointers that stay on 0.3.x until promotion; releases up to 0.3.0-beta.2 are unsigned and refused
- **Docs:** cli/overview.md#install-the-cli

### INS-03 Install a server with a Fleet Token at a hidden prompt
- **Surface:** install
- **Reach:** on the Server: `curl -fsSL https://ployz.sh | sudo sh -s -- <version> --enroll [--key pk1_…]`, paste `pflt_…` at `Fleet Token (from Servers in Ployz Cloud):`
- **Proof:** prints `Installs Docker and Ployz.`, installs the CLI, runs `ployz server checkin --install`, and ends `Waiting for approval` within seconds; `/etc/ployz/fleet-token` is mode 0600 root; `systemctl is-active ployz-checkin` is active; the Server appears under **Waiting to join** at once; the token is in no `ps`, sudo log or shell history; a malformed token ends `A Fleet Token is pflt_ and 43 more letters, digits, - or _; this one is N characters. Copy it again.` with nothing installed; not root ends `Run it as root: …`
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--bare`
- **Compat:** check-in→Cloud: enrollment needs a `ployz` ≥ 0.3.0-beta.6
- **Docs:** servers/add-a-server.md#install-with-a-fleet-token

### INS-04 The install line carries the key the server trusts first
- **Surface:** install
- **Reach:** `curl -fsSL https://ployz.sh | sudo sh -s -- <version> --enroll --key pk1_…` (or `--key=pk1_…`); `--key` with no value
- **Proof:** `/var/lib/ployz-first-key.json` holds the key after the install; `--key` with nothing after it ends `--key needs the key from your install command after it.`; a different key on a rerun asks at the terminal (see SRV-07)
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--bare`
- **Compat:** install line→CLI: `--key` with a 0.3.x version fails, since 0.3 `ployz server checkin` has no `--key`
- **Docs:** servers/add-a-server.md#the-key-on-an-install-line

### INS-05 Install from cloud-init user data
- **Surface:** install
- **Reach:** the **cloud-init** tab's `#cloud-config` `runcmd: - curl -fsSL https://ployz.sh | [PLOYZ_CLOUD_URL='…'] PLOYZ_FLEET_TOKEN=pflt_… sh -s -- <version> [--key pk1_…]` (lines built by `dashboard/src/modules/machines/installer.ts`); in the lab run the same line as root over `ssh host '…'` with no TTY
- **Proof:** with no terminal the install checks in and ends `Waiting for approval`; `PLOYZ_FLEET_TOKEN` is unset before `ployz` runs (absent from `/proc/<pid>/environ`); without the variable and without a terminal it ends `No terminal to read the Fleet Token from. Set PLOYZ_FLEET_TOKEN in the root environment instead, as cloud-init does.`; a Fleet Token in `PLOYZ_ENROLL_TOKEN` is refused naming `PLOYZ_FLEET_TOKEN`
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: local Cloud · External: real cloud-init user data at a provider (the lab simulates it over SSH)
- **Drive:** verify-server `--bare`
- **Compat:** Cloud→install line: the line pins the release Cloud's SDK speaks
- **Docs:** servers/add-a-server.md#install-with-a-fleet-token

### INS-06 Fleet Token install outcomes and reruns
- **Surface:** install
- **Reach:** rerun the INS-03 line: with Cloud unreachable; with a different token; with an expired or revoked token; with a token whose waiting limit is full; with `PLOYZ_CLOUD_URL` pointing at a non-Cloud page; with `/etc/ployz/fleet-token` unreadable
- **Proof:** unreachable Cloud prints the reason and `The check-in service keeps trying.` and exits 0; held back prints `Cloud held this check-in back:` and Cloud's message; a different token while Cloud is down ends `Until Cloud takes the new Fleet Token, this install keeps /etc/ployz/fleet-token and changes nothing else. Run the install again.`; expired prints `Token expired.`; unknown ends `Cloud at … refused this Fleet Token.`; a wrong URL ends `…isn't how Ployz Cloud answers. A proxy or firewall in between may have answered instead`; unreadable file ends `Could not read /etc/ployz/fleet-token`; a joined Server ends `Already joined`; a blocked one prints `Blocked.` per reason
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--bare`
- **Compat:** check-in→Cloud
- **Docs:** servers/add-a-server.md#install-with-a-fleet-token

### INS-07 Point an install or the CLI at your own Cloud
- **Surface:** install
- **Reach:** `curl -fsSL https://ployz.sh | sudo PLOYZ_CLOUD_URL=https://cloud.example.com sh -s -- <version> --enroll`; `ployz login --cloud-url https://cloud.example.com`; `ployz server add --cloud-url …`
- **Proof:** the Server checks in with that Cloud and appears on its **Servers** page; `ployz status` names that Cloud; a URL with a user, password, query or fragment is refused by `login`, `server add` and `server checkin` before anything is stored or sent; a URL that answers but isn't Cloud ends with an error naming the URL
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: local Cloud (the lab Cloud stands in for a self-hosted one)
- **Drive:** verify-server `--bare --cloud`
- **Compat:** CLI→Cloud
- **Docs:** self-hosting.md#4-sign-in-and-add-a-server

### INS-08 Install without ZFS (Docker only)
- **Surface:** install
- **Reach:** `curl -fsSL https://ployz.sh | sudo PLOYZ_SKIP_ZFS=1 sh -s -- <version> --enroll`; `ployz server add --storage none`; Enroll sheet **Volumes → Docker only**
- **Proof:** the Server reads **Docker only** on the Servers page; `zpool list` shows no `ployz` pool; a Volume deploy fails until **Use a plain Docker volume (not recommended)** is ticked
- **Needs:** 1 Machine (`--bare`) · Volumes: Docker · Cloud: local Cloud
- **Drive:** verify-server `--bare`; verify-dashboard SERVERS=1 for the badge
- **Compat:** none
- **Docs:** servers/add-a-server.md#start-without-zfs

### INS-09 Put managed volumes on an empty spare disk
- **Surface:** install
- **Reach:** `sudo ployz server add --storage zfs:/dev/disk/by-id/virtio-data` (or `zfs:/dev/vdb`); `sudo ployzd install --storage zfs:/dev/vdb`; Enroll on arrival and `ployz server enroll` pick a spare disk unless `--no-spare-disk`
- **Proof:** `zpool list` shows `ployz-device` on that disk; a disk with a filesystem, partitions or mounts is refused; after a reboot a volume mount re-imports the pool; with the disk detached, storage requests fail instead of creating a pool on the main disk; a missing previously owned pool makes preparation refuse
- **Needs:** 1 Machine with a second empty disk · Volumes: Managed (ZFS) · Cloud: no (Standalone) · External: a second disk (Incus provider if Boat can't attach one)
- **Drive:** verify-server; verify-pool-ownership
- **Compat:** none
- **Docs:** servers/add-a-server.md#use-an-empty-spare-disk-for-volumes

### INS-10 Build ZFS where the distribution doesn't ship it
- **Surface:** install
- **Reach:** install on Debian 12/13 or Amazon Linux 2023 (`verify-cluster up --bare --distro debian-12`), with a Fleet Token or `ployz server add --storage zfs`
- **Proof:** the waiting row shows **Building ZFS** under **Ready** until done, and a ZFS enrollment waits for it; afterwards `modinfo zfs` loads and `zpool list` works after the first managed Volume; Secure Boot on ends `Secure Boot is on, so this Server can't load the ZFS module…`; an unsupported distro ends `Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023…`
- **Needs:** 1 Machine (`--bare --distro debian-12`) · Volumes: Managed (ZFS) · Cloud: local Cloud for the waiting row
- **Drive:** verify-server `--bare`
- **Compat:** none
- **Docs:** troubleshooting/servers.md#the-install-stops-while-preparing-zfs

### INS-11 Refuse a host that can't run Ployz
- **Surface:** install
- **Reach:** install on a non-systemd host, a non-Linux host with `--enroll`, a disk under 30 GiB, a server without `sudo`
- **Proof:** `A Fleet Token needs a server that runs systemd.` / `A Fleet Token needs a Linux server.`; `Unsupported platform` or `Ployz requires systemd` from `ployz server add`; a small disk is **Blocked** with `Disk is 20 GiB. Ployz needs 30 GiB.`; root without sudo gets `sudo: command not found` and `ployz server add` works without it
- **Needs:** 1 Machine (`--bare`, Incus for a small disk) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--bare` (`--provider incus` for odd hosts)
- **Compat:** none
- **Docs:** troubleshooting/servers.md#my-server-isnt-supported

### INS-12 Stop before changing a Docker Ployz can't use
- **Surface:** install
- **Reach:** on a host with Docker already installed: live-restore off; Docker < 29.2; buildx < 0.18; containerd image store off; Docker stopped; then run the install line or `ployz server add`
- **Proof:** each stops before changing anything with its line, e.g. `Docker's live-restore is off, so your Services stay stopped if Docker crashes. Add "live-restore": true to /etc/docker/daemon.json and run sudo systemctl reload docker…`, `Docker ... is too old: Ployz needs Docker 29.2 or newer…`, `Docker isn't using the containerd image store…`, `Docker is installed but not running…`; after the fix `docker info -f '{{.LiveRestoreEnabled}}'` prints `true` and the rerun completes; a host with no Docker gets a current one installed
- **Needs:** 1 Machine (`--bare` with distro Docker installed first) · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server `--bare`
- **Compat:** upgrade: Servers that joined before the live-restore check (#4584) can still have it off
- **Docs:** troubleshooting/servers.md#the-install-stops-while-checking-docker

### INS-13 Refuse Docker's nftables firewall
- **Surface:** daemon
- **Reach:** set `"firewall-backend": "nftables"` in `/etc/docker/daemon.json` (or run dockerd with `--iptables=false`), restart Docker and Ployz
- **Proof:** `journalctl -u ployz` says `Docker doesn't pass forwarded IPv4 through iptables' DOCKER-USER chain` and names the setting; Ployz doesn't start its network; removing it and restarting Docker and Ployz brings the Server back `up`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** upgrade: the container network fence (#4559) is new in 0.4
- **Docs:** troubleshooting/servers.md#ployz-stops-because-docker-uses-its-nftables-firewall

### INS-14 A server that runs firewalld needs UDP 51820 and ployz-wg trusted
- **Surface:** install
- **Reach:** enable firewalld on a Server, then run a cross-Server command; fix with `firewall-cmd --zone=public --permanent --add-port=51820/udp`, `--zone=trusted --permanent --add-interface=ployz-wg`, `--reload`
- **Proof:** before: a command says `! web-2 did not answer; its rows are missing.` with `cause: Permission denied (os error 13)`; after: `ployz server ls` lists web-2 rows; Ployz's iptables rules cleared by the reload come back within a minute. No installer refusal exists for firewalld (only this documented manual step)
- **Needs:** 2 Machines (Incus or a distro image with firewalld) · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`--provider incus` for a firewalld distro)
- **Compat:** upgrade: 0.3.16 and earlier need `systemctl restart ployz` after the reload
- **Docs:** troubleshooting/servers.md#a-server-that-runs-firewalld-doesnt-answer-your-other-servers

### INS-15 Block a server while another program holds UDP 51820
- **Surface:** install
- **Reach:** hold UDP 51820 (`socat -u UDP-RECV:51820 -` or a `wg-quick` tunnel) on a host, then install with a Fleet Token or `ployz server add`
- **Proof:** the waiting row reads **Blocked** with `UDP port 51820 is in use by socat (pid N).` and can't be selected; `ployz status` lists it under `blocked`; on a joined Server `journalctl -u ployz` says `…so WireGuard can't listen` and Ployz keeps restarting; freeing the port clears it on the next check-in or restart
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server `--bare`
- **Compat:** none
- **Docs:** troubleshooting/servers.md#another-program-holds-udp-port-51820

### INS-16 Install on an IPv6-only server
- **Surface:** install
- **Reach:** run the install line on a host whose only route out is IPv6
- **Proof:** prints `Can't reach https://github.com/getployz/ployz; downloading the release through https://ployz.sh` and installs `ployz`; enrollment can't finish without NAT64/DNS64 (ghcr.io images); with NAT64 the Server sets up and advertises only its own IPv6 addresses
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: an IPv6-only host with NAT64/DNS64 (no lab path exercises NAT64; Boat hosts have a private IPv4)
- **Drive:** verify-server (`--provider incus` with custom IPv6 networks)
- **Compat:** none
- **Docs:** servers/add-a-server.md#ipv6-only-servers

### INS-17 Install from a test copy of the release assets
- **Surface:** install
- **Reach:** `curl -fsSL https://ployz.sh | PLOYZ_GITHUB_URL=https://github.com/<owner>/<repo> PLOYZ_CHANNEL_URL=https://<test> sh -s -- <version>` (also `PLOYZ_FALLBACK_URL`, `PLOYZ_RELEASE_PUBLIC_KEY`)
- **Proof:** downloads `<base>/releases/download/v<version>/…` and still verifies the signature; a version string that doesn't match the release grammar ends `Invalid version: …`; a binary reporting another version ends `Release binary ployz has version X, expected Y`
- **Needs:** 1 Machine (`--bare`) · Volumes: none · Cloud: no (Standalone) · External: a draft or mirrored release (Phase A uses the draft's assets)
- **Drive:** verify-server `--bare`
- **Compat:** none
- **Docs:** servers/manage-servers.md#upgrade-from-the-cli (the variables are described for upgrades; their use with `install.sh` is only mentioned)

### INS-18 Uninstall Ployz from a server
- **Surface:** install
- **Reach:** on the Server: `sudo ployz-uninstall` (`PLOYZ_AUTO_CONFIRM=true` skips the ask)
- **Proof:** asks `Continue with destructive Ployz uninstall?`; ends `Ployz uninstalled; Docker, images, named volumes, and Docker configuration retained`; `docker ps -a --filter label=ployz.managed` is empty, `ployz-wg` is gone, `/etc/ployz/fleet-token` and `/var/lib/ployz-checkin` are gone, `zfs list` still shows volume data; `/var/lib/ployz-first-key.json` reads `"state":"spent"` on an enrolled disk; during an upgrade it ends `Ployz mutation or installation is active; retry uninstall` with nothing removed; reinstalling with a Fleet Token checks in as a new host
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** servers/manage-servers.md#remove-a-server

### INS-19 Configure and start self-hosted Ployz Cloud
- **Surface:** install
- **Reach:** download `ployz-cloud-compose.yml` and `ployz-cloud.env.example` from a release, fill `.env` (APP_URL, generated secrets, GitHub app values), `docker compose run --rm web npm run db:migrate`, `docker compose up -d`, proxy to `127.0.0.1:3000` (`WEB_PORT`, `WEB_BIND`, `COMPOSE_PROJECT_NAME` for a second install)
- **Proof:** `docker compose ps` shows `worker` **healthy**; sign-in with GitHub works at APP_URL; a missing or malformed required variable stops `web` and `worker` with `docker compose logs web worker` naming it; port 3000 isn't reachable from another host; partial PostShiba settings stop startup
- **Needs:** 0 Machines (a Docker host) · Volumes: none · Cloud: self-hosted (not prod) · External: GitHub OAuth App and GitHub App, a public https address
- **Drive:** verify-dashboard (its local Cloud runs the same app but not the compose packaging); otherwise manual on this host
- **Compat:** Cloud self-host: images are `ghcr.io/getployz/ployz-cloud:${PLOYZ_CLOUD_VERSION}` (the source `dashboard/self-host/compose.yml` refuses to start without it; check the release asset sets it)
- **Docs:** self-hosting.md#2-configure

### INS-20 Add a server to a self-hosted Cloud
- **Surface:** install
- **Reach:** self-hosted Cloud → **Add servers** line (already carries `PLOYZ_CLOUD_URL`), or `ployz server token create` from a CLI signed in with `ployz login --cloud-url …`
- **Proof:** the Server checks in on that Cloud's Servers page and joins; the dashboard line installs Cloud's release, the CLI's line its own release; Cloud reaches the Server through `relay.ployz.dev`
- **Needs:** 1 Machine · Volumes: none · Cloud: self-hosted (the `--cloud` lab Cloud stands in) · External: the production relay for the hosted path
- **Drive:** verify-server `--cloud`
- **Compat:** CLI→Cloud, Cloud→Server: a CLI and Cloud on different releases install different Server releases
- **Docs:** self-hosting.md#4-sign-in-and-add-a-server

### INS-21 Upgrade self-hosted Cloud, and refuse an older image
- **Surface:** install
- **Reach:** the upgrade script in self-hosting.md with `VERSION=vX.Y.Z` (fetch compose, `pull`, `db:migrate`, `up -d`); then start an older image against the migrated database; `PLOYZ_ALLOW_OLDER_IMAGE=1`
- **Proof:** the script refuses a tag older than the running one (`… is older than …. Not downgrading.`); after the upgrade `docker compose config --images` shows the new tag and pages load; an older image's `db:migrate`, `web` and `worker` refuse and name the image version; with `PLOYZ_ALLOW_OLDER_IMAGE=1` it starts with a warning
- **Needs:** 0 Machines (a Docker host) · Volumes: none · Cloud: self-hosted · External: GitHub apps and https address from INS-19
- **Drive:** manual on this host (no verify skill covers compose packaging)
- **Compat:** Cloud self-host, upgrade: 11 drizzle and Config Store 0047–0059 migrations (incl. DROP TABLE) between 0.3 and 0.4; a beta.3 → beta.2 rollback likely breaks on 0057
- **Docs:** self-hosting.md#upgrade

### INS-22 Reseal secrets when upgrading self-hosted Cloud from v0.3.4 or earlier
- **Surface:** install
- **Reach:** upgrade a self-hosted Cloud that holds secret variables saved on v0.3.4 or earlier, keeping `APP_ENCRYPTION_SECRET`
- **Proof:** `docker compose logs worker` shows the reseal and then `0 legacy values` per Organization; a deploy before it finishes stops saying the variable is waiting to be resealed; after it, the deploy succeeds with the same values
- **Needs:** 1 Machine · Volumes: none · Cloud: self-hosted · External: a v0.3.4 self-host database to start from
- **Drive:** manual (verify-server `--cloud` can't start from an old database)
- **Compat:** Cloud self-host, upgrade
- **Docs:** self-hosting.md#upgrading-from-v034-or-earlier

### INS-23 Keep logs, metrics and traces in self-hosted Cloud
- **Surface:** install
- **Reach:** set `CLICKHOUSE_PASSWORD`, `docker compose --profile observe up -d`
- **Proof:** `docker compose --profile observe ps` lists ClickHouse and the Collector, neither publishing a port; the Logs page reads history instead of **Log history isn’t set up**; without the profile a Server sending to Cloud gets `503` and keeps its batch; a Collector restart loses nothing (`observe-collector-queue` volume)
- **Needs:** 1 Machine · Volumes: none · Cloud: self-hosted (or the `--cloud` lab Cloud with the observe stack)
- **Drive:** verify-server `--cloud`; see observe.md for what the pages show
- **Compat:** Cloud→Server: 0.3 Servers run no Collector, so they show no history in main Cloud
- **Docs:** self-hosting.md#keep-logs-metrics-and-traces-in-cloud-optional
