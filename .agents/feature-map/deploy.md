# Deploy

Staging changes, deploying them from the dashboard, the CLI, CI and agents, following and inspecting Deployments, history and rollback, signed versions and how a Server admits them, Docker image services, deploy-on-push from GitHub and its automatic deploy rule, and how a deploy replaces replicas without downtime. Builds themselves, the GitHub connection and the build order are in [builds.md](builds.md). Verifiers start with `ployz deploy` on a Standalone Boat Cluster (DEP-04, DEP-36), then the dashboard bottom bar on a local Cloud (DEP-01 to DEP-03); [changes-and-deploy.md](../skills/verify-dashboard/features/changes-and-deploy.md) maps the dashboard handles. Approval entries (DEP-13 to DEP-24) need a 0.4 Cluster, since 0.4 Servers enforce signed versions and an all-0.3.19 Cluster reads as "off".

### DEP-01 Stage a change and see it counted in the bottom bar
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production`, open a service, change a setting under **Settings** (handles in [changes-and-deploy.md](../skills/verify-dashboard/features/changes-and-deploy.md))
- **Proof:** the field turns pink and its hover shows the running value; the bottom bar reads **N changes**; after a reload the change is still there and the running container is unchanged (`ployz ps`); **Auto-deploy**, **Wait for CI**, **Watch paths**, **Preferred Builder** apply at once and are not counted
- **Needs:** 0 Machines (1 with SERVERS=1 to prove nothing restarted) · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** deploy/deployments.md#stage-changes

### DEP-02 Review staged changes from the CLI
- **Surface:** CLI
- **Reach:** `ployz set web.image=nginx:1.27 && ployz diff` (also `--json`; MCP `review`)
- **Proof:** `ployz diff` lists `web.image: "nginx:1" → "nginx:1.27"` with the latest saved version; a new service's row reads `web.image: null → "nginx:1"`; `--json` carries the version `ployz discard --version` and `deploy --expect-version` take; MCP `review` returns the same rows plus what the deploy destroys
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: a 0.3.19 CLI reads main's Config Store
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-03 Review changes in Details and deploy them from the dashboard
- **Surface:** dashboard
- **Reach:** bottom bar **Details** → type a **Message (optional)** → **Deploy changes**; or **Deploy** in the bar to skip the review
- **Proof:** the new Deployment's page opens with heading `#N · <message>`; **Deployments** lists the row; with SERVERS=1 the bar shows **Deployed** for a few seconds and the canvas cards lose their pink fields; a member in a Protected Environment sees **Request deploy** instead
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud (SERVERS=1)
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: against an all-0.3.19 Cluster the deploy goes unsigned and applies
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-04 Deploy everything staged from the CLI
- **Surface:** CLI
- **Reach:** `ployz deploy` (also `--message TEXT`, `--project`, `--env`, `--json`; MCP `deploy`, which returns once queued)
- **Proof:** live block ends `switched N/N` and prints **Live**; exit 0; `ployz deployment ls` shows the new row `applied`; every GitHub service moved to its branch head even with nothing staged; a service with no source refuses with `api has nothing to run yet: add an image, connect a repository, or run ployz up in its directory`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-region-placement for where replicas land
- **Compat:** CLI→Cloud: 0.3.19's admit fields are a subset of main's and deploy; a beta.2 CLI is refused with "Update ployz"
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-05 Deploy only some services
- **Surface:** CLI
- **Reach:** `ployz deploy web` where `web` references `${{ api.URL }}`; then `ployz service rm big && ployz deploy big`
- **Proof:** the Deployment lists `web` and `api` (with `api`'s saved changes); other services keep their containers (`ployz ps` creation times unchanged); `ployz deploy big` fails with `Service big is staged for removal; a full Deploy removes it`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-06 A full deploy prunes services the environment doesn't hold
- **Surface:** CLI
- **Reach:** start an extra service outside the environment (or `ployz service rm db`), then `ployz deploy --plan`, `ployz deploy`; then `ployz deploy --no-prune` (also `ployz up --no-prune`, dashboard **⋮ → Deploy, keep other services**)
- **Proof:** `--plan` warns it removes them; `ployz deploy` prints `db: removed 1/1 (not in this environment)` and the volume stays (`zfs list`); `--no-prune` prints `postgres: running, not in this environment; kept.`; with a Server partitioned, `Prune skipped: not every Server answered`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server (`partition on` for the skip)
- **Compat:** none
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-07 A Deploy token can't prune, and an agent's prune asks first
- **Surface:** CLI
- **Reach:** with a Deploy-role Organization token in `PLOYZ_TOKEN`, run `ployz deploy` while an extra service runs; then an agent `deploy` (MCP) with the same extra service
- **Proof:** refused with `Needs changes:destroy: this deploy removes extra2, running in this environment but not part of it. Deploy with --no-prune to keep them.`; redeploying a pruning deployment says `Have an owner or admin redeploy #16.`; the agent's review lists **Removes service extra2**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud
- **Compat:** CLI→Cloud
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-08 Removing a service while a Server is down
- **Surface:** CLI
- **Reach:** `web` with 3 replicas on 3 Machines; `verify-cluster power off $run machine-2`; `ployz service rm web && ployz deploy`
- **Proof:** prints `web: removed 2/3; machine-2 didn't answer, the next full deploy removes its copy once it is back` and exits 3; after `power on` and another `ployz deploy`, `ployz ps` on machine-2 shows no `web` container
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server↔Server
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-09 Save changes without deploying
- **Surface:** dashboard
- **Reach:** **Details → Save only**, **⋮ → Save only**, or Alt-click **Deploy**; CLI `ployz deploy --skip-deploy --message "x"`; MCP `deploy` with `skip_deploy`
- **Proof:** the bar shows **Saved, not deployed**, then **Save #4 not deployed** with **Deploy**; **Versions** gains #4 with the message; no new Deployment row; saving again with no change adds no version; after a refused deploy the bar reads **Save #4 · its deploy ran nothing** and `ployz diff` says `Saved as version #4. Its deploy ran nothing, so none of these changes reached a server.`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-10 Discard one staged change or all of them
- **Surface:** CLI
- **Reach:** `ployz discard web.env.KEY`; `ployz discard`; `ployz discard --target review`; `ployz discard web.image` on a new service (also dashboard row **⋮ → Discard** and **⋮ → Discard all changes**; MCP `discard`)
- **Proof:** a path drops only that row from `ployz diff`; plain `discard` lists each dropped change, including dashboard-staged ones, and `ployz diff` is empty; `--target review` returns to the latest saved version; `discard web.image` names `ployz discard web`; a discarded never-deployed service is gone from `ployz get`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-dashboard for the buttons
- **Compat:** none
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-11 A deploy skips Not set secrets and broken references
- **Surface:** CLI
- **Reach:** `ployz set web.env.TOKEN --secret` left Not set and `web.env.API=${{ gone.URL }}`, then `ployz deploy`
- **Proof:** the deploy succeeds and warns naming `TOKEN` and the broken `API` reference; `ployz exec web -- env` has neither variable
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** deploy/deployments.md#deploy-your-changes

### DEP-12 Recreate every container of a deploy
- **Surface:** CLI
- **Reach:** `ployz deploy --recreate` (or `ployz deploy --recreate web`)
- **Proof:** `ployz ps` shows new container IDs and start times for every named service even with nothing staged
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** upgrade: the documented step after upgrading Servers to give every container its new address
- **Docs:** services/private-networking.md#right-after-upgrading

### DEP-13 Sign a deploy on an approved device
- **Surface:** CLI
- **Reach:** on an approved device, `ployz deploy` for a service with a sealed variable and a Managed volume; answer `n`, then again `y`
- **Proof:** prints `Approved web: nginx:1.29-alpine → @sha256:…` with the signed lines, then `Approve web in namespace shop-production? [y/N]`; `n` exits 130 and `ployz deployment ls` has no new row; `y` deploys; outside a terminal it refuses before signing
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud (`verify-cluster cli --device`) or Standalone
- **Drive:** verify-server; verify-approval-binding
- **Compat:** Cloud→Server: an all-0.3.19 Cluster reads as "off", so nothing is signed and Cloud deploys unsigned
- **Docs:** deploy/deployments.md#deploy-from-an-approved-device

### DEP-14 A signed deploy checks each volume mount's data
- **Surface:** CLI
- **Reach:** deploy a service with a Managed volume twice from an approved device; then point the mount at another volume and deploy; then deploy through Cloud alone (no `--connect`)
- **Proof:** the second deploy folds the mounts into one line like `2 volumes keep the data you approved (signed 3 Nov 14:02 UTC)`; the swap prints `Needs your yes:` with `different data than the version you approved` and `Use the data marked above? [y/N]`; through Cloud alone it warns, prints every mount in full and refuses a pre-0.4 volume
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server --cloud; verify-approval-binding
- **Compat:** upgrade: a volume from before 0.4 passes only when the approved version or a running container vouches for it
- **Docs:** deploy/deployments.md#deploy-from-an-approved-device

### DEP-15 A deploy pins a public tag to its digest and refuses a private tag
- **Surface:** CLI
- **Reach:** `ployz set web.image=nginx:1.29-alpine && ployz deploy`; then a private `ghcr.io/…:tag`
- **Proof:** the signed line reads `nginx:1.29-alpine → @sha256:…` and `docker inspect` on the Server shows that digest while `ployz get web.image` keeps the tag; the private tag stops before queueing with `ghcr.io wants credentials to read web's image, so ployz can't pin it. Write its digest.`
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone) · External: a private registry image
- **Drive:** verify-server
- **Compat:** none
- **Docs:** deploy/deployments.md#deploy-from-an-approved-device

### DEP-16 Each Server admits only signed versions of services that reach data
- **Surface:** daemon
- **Reach:** deploy a volume-mounting service from an approved device; then deploy it with an Organization token only (or `ployz up`); then deploy from a device whose clock runs 11 minutes ahead
- **Proof:** `journalctl -u ployz` logs `web admitted by signed version from [Device(…)] digest=…`; the unsigned deploy fails with `This service mounts a volume, so this server runs it only from a deploy an approved device signed.` and the old container keeps running; the future-clock signature is refused
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server --cloud; verify-approval-binding; verify-service-floor
- **Compat:** Cloud→Server: 0.3.19 Servers ignore `versions` and run unsigned specs
- **Docs:** deploy/deployments.md#deploy-from-the-dashboard

### DEP-17 Ship a new image without a terminal
- **Surface:** CLI
- **Reach:** `ployz deploy web --image web=ghcr.io/acme/web:1.5 --yes` (or `PLOYZ_AUTO_CONFIRM=1`); repeat with another staged change such as `api.startCommand`
- **Proof:** prints what it signs, deploys, and `ployz get web.image` shows the new image; with the extra change it stops with `error: api has a staged change at api.startCommand that no person approved. … Nothing signed or deployed.` and `next: ployz deploy api`
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server --cloud
- **Compat:** CLI/Cloud→Server: a silent deploy needs `parent-versions.v1` on every Server, so a mixed cluster with a 0.3.19 Server refuses it
- **Docs:** deploy/deployments.md#deploy-a-new-image-without-a-terminal

### DEP-18 Deploy from CI with a CI key
- **Surface:** CLI
- **Reach:** `ployz key add --ci` on an approved device, then `PLOYZ_TOKEN=… PLOYZ_KEY=… ployz deploy web --image web=<image>` from a machine with no device key
- **Proof:** deploys with no `--yes`; a change outside `--image` or outside the key's scope is refused; removing the approving device makes the next CI deploy fail
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server --cloud; verify-approval-binding
- **Compat:** CLI/Cloud→Server: needs `parent-versions.v1` on every Server; a 0.3.19 Server is named for upgrade
- **Docs:** deploy/deployments.md#deploy-from-ci-with-a-key

### DEP-19 Approve a keyless deploy from a link with a passkey
- **Surface:** dashboard
- **Reach:** on a computer with no device key, `ployz deploy` (or `ployz up`, or an agent's `deploy`); open the printed `/cloud/ada/approve/<proposalId>` and tap
- **Proof:** CLI prints `Approve this deploy at …/approve/…` then `Waiting for approval… approved.` and follows; `--json` returns `proposal_id` at once; no tap in 10 minutes stops with `Nothing deployed yet`; the page lists what it destroys and the signed lines; an upload approved after a day deploys nothing
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: a passkey the Servers trust (the lab has no passkey recipe)
- **Drive:** verify-dashboard SERVERS=1; verify-approval-binding
- **Compat:** CLI→Cloud
- **Docs:** cli/overview.md#deploy-without-a-key

### DEP-20 Refuse to sign when a Server can't check signed versions
- **Surface:** CLI
- **Reach:** a Cluster with one Server on 0.4.0-beta.2 and one on the candidate; `ployz deploy` from an approved device
- **Proof:** `error: Upgrade web-1 first: it enforces device approval with an older ployz that can't check this device's signature, so it would refuse this deploy. Nothing signed or deployed.` with `next: ployz server upgrade beta web-1`; a partitioned enforcing Server gives `Couldn't confirm web-2 can check this device's signature`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`--daemon machine-1=0.4.0-beta.2`)
- **Compat:** CLI→daemon: probes `ployz.container.signed-version.v1`; beta.2 Servers lack it
- **Docs:** deploy/deployments.md#upgrade-servers-before-you-sign

### DEP-21 After a Server upgrade, old containers need one signed deploy
- **Surface:** daemon
- **Reach:** 0.3.19 Cluster with a volume service; upgrade Servers to the candidate; `ployz service stop db && ployz service start db`; then `ployz deploy` signed
- **Proof:** the start is refused with `This container was made before this server's upgrade …`; a stop-first deploy that fails reads `Could not restore the old Container.`; after the signed deploy each service runs a new container (`ployz ps` new IDs) and restarts work
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server ("Roll one Server"); verify-service-floor
- **Compat:** upgrade: 0.3.19→0.4 enforces at once; every service needs one signed deploy
- **Docs:** deploy/deployments.md#after-a-server-upgrade-deploy-each-service-once

### DEP-22 Deploy from the dashboard with a passkey
- **Surface:** dashboard
- **Reach:** stage a change to a volume service, **Deploy** → sheet → **Approve with passkey**; also **Create and deploy**, **Deploy again**, log card **Redeploy**
- **Proof:** the sheet lists each signed service with image by digest and **Needs your yes** lines; the deploy starts after one prompt; refusals before the prompt: `web pulls a private tag. Write its image by digest (image@sha256:…).`, `Upgrade web-1 first. Its ployz can't check your passkey.`, `Couldn't reach web-1. Try again.`, `Your servers don't trust your passkey. …`; **Cancel** keeps changes staged
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: a passkey the Servers trust
- **Drive:** verify-dashboard SERVERS=1; verify-approval-binding
- **Compat:** Cloud→Server
- **Docs:** deploy/deployments.md#deploy-from-the-dashboard

### DEP-23 Forget older versions
- **Surface:** CLI
- **Reach:** `ployz deploy api --forget-older` at a terminal on an approved device; then **Redeploy** an older deployment of `api`
- **Proof:** the command prints each Server's answer; the older redeploy fails at admission while running containers stay up; a powered-off Server is named and the command exits 3; automatic deploy rules for `api` stop until **Approve again**
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server --cloud; verify-approval-binding
- **Compat:** CLI→daemon: a Server too old to forget is named and the command exits 3
- **Docs:** deploy/history-and-rollback.md#forget-older-versions

### DEP-24 Deploy every image pushed to one repository without a prompt
- **Surface:** dashboard
- **Reach:** environment **Settings → Automatic deploys** → **Deploy every image pushed to ghcr.io/acme/web** (passkey); then `ployz set 'web.image=ghcr.io/acme/web@sha256:…' && ployz deploy web` from CI
- **Proof:** the digest deploys with no prompt; a tag, another repository or a re-served digest is refused; a failed deploy after the pre-deploy command makes a retry of the same image refused; turning on **Deploy every push** turns this off; Servers older than this release refuse and say to upgrade
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: a passkey, a public registry repository you can push to
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: older Servers refuse a deploy under the rule
- **Docs:** deploy/docker-image.md#deploy-each-new-image-without-a-prompt

### DEP-25 Add a service from a Docker image
- **Surface:** dashboard
- **Reach:** canvas **Create → Docker image** → `traefik/whoami` → **Continue** → **Settings → Networking → Generate Domain**, **Port** 80 → **Deploy**; change it later under **Settings → Source → Container image**
- **Proof:** with SERVERS=1 the service reads Online and `curl https://<generated domain>` answers; an image declaring ports with no `PORT` set makes the deploy warn and name them
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** deploy/docker-image.md#add-a-service-from-an-image

### DEP-26 Ship a new version of an image from CI
- **Surface:** CLI
- **Reach:** `PLOYZ_TOKEN=… ployz set 'web.image=ghcr.io/acme/web:1.5.0' --project shop --env production && ployz deploy web --project shop --env production`
- **Proof:** `docker inspect` on the Server shows the new digest; redeploying the same mutable tag keeps the old copy
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud
- **Compat:** CLI→Cloud
- **Docs:** deploy/docker-image.md#ship-a-new-version-of-your-image

### DEP-27 Servers pull by digest and only you write tags
- **Surface:** daemon
- **Reach:** deploy `nginx:1.27` through Cloud; then deploy it with `--connect ssh://root@machine-1`
- **Proof:** after the Cloud deploy, `docker image ls` on the Server shows the image with no `nginx:1.27` tag; after the direct deploy the tag exists on machine-1 only; a `pull: never` service on machine-2 runs only an image machine-2 pulled itself
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud
- **Compat:** CLI/Cloud→Server: an older Cloud or CLI asking to store a tag is refused naming `ployz.image.pull-from-machine.v1`
- **Docs:** deploy/docker-image.md#tags-on-your-servers

### DEP-28 Use a private image with sealed registry credentials
- **Surface:** dashboard
- **Reach:** **Settings → Source → Registry credentials → Add credentials**; CLI `ployz set web.registryCredential --secret --username octo --registry ghcr.io`
- **Proof:** the row reads like **GitHub Container Registry · ghcr.io**; the deploy pulls the private image; **Disconnect** then deploy fails at pull; the Config Store holds only a sealed blob
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: a private registry and token
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** CLI→Cloud: a 0.3.19 CLI that sets a registry login is refused (store migration 0055) and told to update
- **Docs:** deploy/docker-image.md#use-a-private-image

### DEP-29 A Server won't send registry credentials where they could leak
- **Surface:** daemon
- **Reach:** with credentials set, add the registry to `insecure-registries` in `/etc/docker/daemon.json` (or a `registry-mirrors` entry for a Docker Hub image, or write `GHCR.io/acme/web`), restart Docker, deploy
- **Proof:** **Failed at pull** with `this Server won't pull the image with its registry login` and a cause such as `Docker on this Server reaches … without TLS`; the old version keeps running
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: a private registry
- **Drive:** verify-server --cloud
- **Compat:** none
- **Docs:** deploy/docker-image.md#use-a-private-image

### DEP-30 A bad image fails at pull and keeps the old version
- **Surface:** CLI
- **Reach:** `ployz set web.image=nginx:1.27-alpne && ployz deploy`
- **Proof:** fails with `Image nginx:1.27-alpne not found, or its registry needs credentials`, offers `undo: ployz history undo N` and no `retry:`; exit 1; the old container still serves; a nearly full disk shows `Server machine-3's disk is full: … free under /var/lib/containerd`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/deployments.md#my-image-isnt-found

### DEP-31 Override the start command
- **Surface:** dashboard
- **Reach:** **Settings → Deploy → Start command** `sh -c 'echo hi && exec nginx -g "daemon off;"'`, deploy; then `npm run migrate && npm start` (CLI `ployz set web.startCommand=…`)
- **Proof:** `docker inspect` shows the entrypoint replaced with no shell wrapper; the `&&` form runs only the first command; clearing the field restores the image's command
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** deploy/docker-image.md#override-the-start-command

### DEP-32 One deployment at a time: Queued, Deploy next, Superseded
- **Surface:** dashboard
- **Reach:** start a slow deploy, then **Deploy next** twice
- **Proof:** the bar reads **Deploy next**; the first waiting deploy reads **Superseded** and the newest **Queued**; `ployz deploy` on a queued one prints `Deployment #2 waits for Deployment #1 to finish.` after 30 s; an out-of-queue deploy that loses fails with `A newer deploy is already serving my-app-production/web.`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1; verify-runner-standing
- **Compat:** none
- **Docs:** deploy/deployments.md#when-another-deployment-is-running

### DEP-33 Follow a deployment on its page
- **Surface:** dashboard
- **Reach:** **Deployments** (Runs) `/cloud/ada/shop/production/deployments` → a row → `/deployments/$deploymentId`
- **Proof:** one card per build, service, volume and config in deploy order; a failed deployment opens on the failed card with the reason in red; statuses from the table (Queued, Preparing, Deploying, Deployed, **Deployed, 2 waiting**, Failed, Cancelled, Superseded, Unknown) match `ployz deployment ls`; **Failed** in the bar survives a reload; **Fix it on a branch** shows on a failed one
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server
- **Docs:** deploy/deployments.md#follow-a-deployment

### DEP-34 Cleanup of old replicas after a dashboard deploy
- **Surface:** dashboard
- **Reach:** dashboard deploy of a 2-Server service while `verify-cluster partition on $run machine-2`; open the deployment
- **Proof:** a **Cleanup** card after the last service reads **Waiting**, then **Left running on machine-2** after three tries; after `partition off` and another deploy it reads **Done**; a CLI deploy shows no Cleanup card
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2; verify-runner-standing
- **Compat:** Cloud→Server
- **Docs:** deploy/deployments.md#cleanup

### DEP-35 List and inspect deployments from the CLI
- **Surface:** CLI
- **Reach:** `ployz deployment ls --limit 5`; `ployz --json deployment show 37` (MCP `deployment_list`, `deployment_show` with `wait_seconds`)
- **Proof:** `ls` lists newest first with status and saved revision; `show --json` has `nodes[].rows[]` with `machine_id`, `state`, times, scrubbed log tails on failures, `live_at` in Unix seconds and `steps[]` with `started_at`/`ended_at`; text ends with step timings like `building api  +2.1s  41.3s`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: older CLIs read `cancelled`/`interrupted` rows as failed
- **Docs:** cli/overview.md#follow-and-inspect-deployments

### DEP-36 Follow a deploy in the CLI with exit codes
- **Surface:** CLI
- **Reach:** `ployz deploy` at a terminal; `ployz deploy 2>&1 | cat`; `ployz deploy --events ev.ndjson`; `ployz deploy --detach`; Ctrl-C during a Cloud deploy
- **Proof:** rows move `waiting 0/1 (pulling <image>)` → `booting` → `switching` → `switched`; piped output prints `still waiting:` after 30 s; `ev.ndjson` lines carry the Deployment `id` and `number`; `--detach` returns the queued Deployment; a failure exits 1 with `inspect:`; a left-running copy or a waiting service exits 3; Ctrl-C exits 130 and the deployment keeps running
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: before 0.3.10 an unrecognized-membership Server printed its ID and exited 0
- **Docs:** troubleshooting/deployments.md#following-a-deploy-in-the-cli

### DEP-37 Cancel a deployment
- **Surface:** CLI
- **Reach:** `ployz deployment cancel 7` (dashboard **Cancel**, **⋮ → Cancel deployment…** on a queued one; MCP `deployment_cancel`)
- **Proof:** a queued one never runs; a running one reads **Cancelled**, its stopped rows read `cancelled` and `--json` keeps `"state": "failed"` with `"cancelled": true`; the following `ployz deploy` ends with `Deployment #7 was cancelled.`; cancelling an ended one fails
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud; verify-runner-standing
- **Compat:** CLI→Cloud
- **Docs:** deploy/history-and-rollback.md#redeploy-or-cancel-a-deployment

### DEP-38 Start a queued deployment now
- **Surface:** CLI
- **Reach:** `ployz deployment start 8` (dashboard **Deploy now**; MCP `deployment_start`)
- **Proof:** the queued Deployment moves to Preparing and the CLI follows it; with no Server in the Organization it says `This Organization has no servers yet, so nothing deployed. Add one, then deploy again.`; a running one fails
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud; verify-runner-standing (`runner kill` to strand a queued one)
- **Compat:** CLI→Cloud
- **Docs:** cli/overview.md#follow-and-inspect-deployments

### DEP-39 Redeploy a finished deployment
- **Surface:** dashboard
- **Reach:** **Redeploy** on the newest finished deployment; CLI `ployz deployment redeploy 6`; MCP `deployment_retry`
- **Proof:** a new Deployment ships #6's saved revision and commits; staged changes stay staged; **Copies wait for Deploy** shows when volume changes are staged; while #5 runs, `Wait for #5 to finish, then redeploy.`; a failed restore refuses with `Restore failed. Restore again.`; an applied removal can't be redeployed
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: sends the signatures Cloud kept; a redeploy of an unsigned deployment is refused for data-reaching services
- **Docs:** deploy/history-and-rollback.md#redeploy-or-cancel-a-deployment

### DEP-40 Roll back to an older deployment
- **Surface:** dashboard
- **Reach:** **Redeploy** on #4 when #5 and #6 ran → **Roll back to #4**; CLI `ployz deployment redeploy 4 --yes` (plus `--accept-volume-loss <vol> --expect-version <v>` when it deletes data)
- **Proof:** the dialog names **Undoes #5 and #6**; services added after #4 stop and come back as staged changes; the CLI refuses outside a terminal without `--yes` (`confirmation_required`) and prints the accept command for data loss; a rule-built service rolls back while the rule's approval stands
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1; verify-server --cloud
- **Compat:** Cloud→Server: a pre-signing deployment's rollback runs the images Cloud kept and signs only versions a device signed since
- **Docs:** deploy/history-and-rollback.md#redeploy-or-cancel-a-deployment

### DEP-41 Undo or roll back a saved version in Versions
- **Surface:** dashboard
- **Reach:** **Deployments → Versions** (`/cloud/ada/shop/production/deployments/versions`) → a version's **⋮ → Undo these N changes** or **Roll back to #N**; a row's **⋮**
- **Proof:** the result stages on the canvas and the bar counts it; **Live** and **Failed** badges match `ployz history`; an action over draft changes lists the overwritten fields; a deleted sealed variable comes back **Sealed**, **Not set** with a toast; a sealed-only version's Undo reads **Sealed. Nothing to undo.**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** deploy/history-and-rollback.md#undo-or-roll-back-a-saved-version

### DEP-42 Undo or restore a saved version from the CLI
- **Surface:** CLI
- **Reach:** `ployz history`; `ployz history restore 2 --preview`; `ployz history undo 2`, then with `--accept-overwrite`; MCP `history`
- **Proof:** `ployz history` lists versions with author, changed settings, message and `(live)`; `--preview` writes nothing (`ployz diff` unchanged); an undo over draft fields refuses naming them and the `--accept-overwrite` command; `--json` lists `not_set` variables
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** cli/overview.md#follow-and-inspect-deployments

### DEP-43 Confirm a deploy that deletes data
- **Surface:** dashboard
- **Reach:** delete a volume, **Deploy** → **Deletes data** → type `shop/production`; also **Save only** (**Next deploy deletes data**); CLI `ployz deploy` then the printed `ployz deploy --accept-volume-loss pgdata --expect-version …`
- **Proof:** the dialog lists the volume; after confirming the dataset is gone (`zfs list`); a confirmed Save lets the next deploy, or a push, delete without asking; the CLI refuses and prints the exact command; a powered-off holder makes it refuse naming the Server and delete nothing
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2; verify-removal-marks
- **Compat:** none
- **Docs:** deploy/deployments.md#confirm-a-deploy-that-deletes-data

### DEP-44 Start-first replace keeps the app serving
- **Surface:** daemon
- **Reach:** a 2-replica HTTP service with **Healthcheck** `/`; run `while curl -fsS https://<domain>/; do :; done` while `ployz deploy` changes a variable
- **Proof:** the curl loop sees no failure; `ployz ps` briefly lists old and new copies; old copies stop within 3 s of the switch; a redirecting health check fails the deploy; no health check stops the old copy about 1 s after the new one starts
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** deploy/zero-downtime.md#keep-your-app-serving-through-a-deploy

### DEP-45 Choose the update order
- **Surface:** dashboard
- **Reach:** **Settings → Deploy → Update order**: **Start first**, **Stop first**, **Rolling**, **Automatic** (CLI `ployz set web.updateOrder=…`); set **Start first** on a service with a volume
- **Proof:** Stop first shows a gap in the curl loop; Rolling replaces one Server at a time and a failed Server keeps later Servers on the old version; Start first on a volume service is refused saying to choose **Stop first**; `ployz deploy` shows `rolling back (1 failed)` when a new copy fails
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2; verify-server with `probe` for the Stop first gap
- **Compat:** none
- **Docs:** deploy/zero-downtime.md#choose-how-a-deploy-replaces-replicas

### DEP-46 Pre-deploy command and dependency order
- **Surface:** daemon
- **Reach:** `web` with **Pre-deploy command** `sh -c 'exit 1'` (then `sleep 400`), referencing `${{ postgres.DATABASE_URL }}`; `ployz deploy`
- **Proof:** the failing command fails the deployment and the old `web` keeps serving; past 5 minutes it fails the same way; in a passing deploy `postgres` switches (and passes its health check) before `web` starts, per `deployment show` step times
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** deploy/zero-downtime.md#keep-your-app-serving-through-a-deploy

### DEP-47 A deploy when a Server is cut off
- **Surface:** daemon
- **Reach:** a 3-Server service; `verify-cluster partition on $run machine-2` (or `power off`) before or during `ployz deploy`
- **Proof:** exits 3 with `my-app-production/web copy … left running on machine-2 (machine-2 did not answer). The next deploy cleans it up.` (or the `If machine-2 restarts and confirms …` line); after `partition off` and another deploy the old copy is gone from `ployz ps`; `ployz status` reports `web = machine-2 has not picked up the new copies` while it lags
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server↔Server
- **Docs:** deploy/zero-downtime.md#when-a-server-is-cut-off-during-a-deploy

### DEP-48 A deploy survives ployzd restarting mid-deploy and mid-retirement
- **Surface:** daemon
- **Reach:** during `ployz deploy`, `systemctl restart ployz` on the Entry Server; repeat on another Server during the old-copy retirement; repeat on a Stop first service
- **Proof:** the deploy reconnects within a minute and finishes, or fails with `cause: connection to the daemon closed` and exits 1 having removed its new copies; a restart during retirement leaves no old copy (`ployz ps`) and no `not seen gone`; the next `ployz deploy` finishes a failed switch
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server (fault build for exact timing); verify-runner-standing
- **Compat:** upgrade: a Server on an earlier release keeps the connection open and the deploy hangs on `still waiting:`
- **Docs:** troubleshooting/deployments.md#ployz-restarted-on-a-server-during-my-deploy

### DEP-49 Deploy a directory with ployz up
- **Surface:** CLI
- **Reach:** in a Node app's Git checkout, `ployz up` (also `--server root@<ip>`, `--detach`, `--no-prune`); in an empty directory
- **Proof:** creates and links a Project named after the directory, uploads, builds on a Server and prints the https address that answers; `.env` listed in `.gitignore` isn't in the upload (`ployz exec web -- ls`); an empty directory stops with `Nothing to deploy` and exit 2; an upload over 95 MiB is refused; on Servers that enforce approval a data-reaching service fails at build with `This server runs only builds an approved device starts.`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon
- **Docs:** cli/overview.md#deploy-a-directory

### DEP-50 Add a service from a GitHub repository
- **Surface:** dashboard
- **Reach:** **Projects → New project → GitHub repository** (or canvas **Create → GitHub repository**) → pick `acme/blog` → **Deploy**
- **Proof:** the service is named `blog`, follows the default branch, and the Deployment has a build card; with no app, typing `owner/repo` offers **Deploy owner/repo**, and `ployz set web.waitForCi=true` prints `Pushes reach Ployz only once the GitHub App is installed on owner/repo.`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1 covers it)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** deploy/github.md#add-a-service-from-a-repository

### DEP-51 Choose the Git branch and protect who changes it
- **Surface:** dashboard
- **Reach:** **Settings → Source → Branch** pencil → pick `feature`; as a member in a Protected Environment try the same
- **Proof:** the change stages and the next deploy builds the `feature` head commit; the member can't change it; a PR into the branch shows **Merging deploys to production, a Protected Environment.** on its **Ployz · ready to merge** check; a deleted branch fails with `The source branch no longer exists.`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** deploy/github.md#choose-the-git-branch

### DEP-52 Deploy on push
- **Surface:** dashboard
- **Reach:** **Auto-deploy** on; send `{"event":"push","installation":7,"repository":"acme/blog","ref":"refs/heads/main"}` to the fake GitHub; then turn **Auto-deploy** off and push again
- **Proof:** a Deployment appears in **Runs** for the pushed commit shipping saved (not staged) changes; with Auto-deploy off, or another branch pushed, none appears
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** Cloud→Server
- **Docs:** deploy/github.md#deploy-on-push

### DEP-53 Approve an automatic deploy rule in the dashboard
- **Surface:** dashboard
- **Reach:** environment **Settings → Automatic deploys → Deploy every push** → **Save with passkey**; **Branch**; **Approve again**; Organization → Builds **Add workflow**
- **Proof:** the sheet lists repository, branch, builder, servers and each service's settings; afterwards pushes deploy with no prompt; a CLI save of a covered setting shows **Pushes to main are on hold** until **Approve again**; after adding a Server a volume service waits until approved again; turning it off makes a push's build fail
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud · External: a passkey; real GitHub Actions for a pushed build (FAKE_GITHUB=1 covers the sheet and dispatch)
- **Drive:** verify-dashboard SERVERS=2 FAKE_GITHUB=1; verify-build-handoff
- **Compat:** Cloud→Server: a Server older than the rule refuses it
- **Docs:** deploy/github.md#approve-a-rule-for-deploys-on-push

### DEP-54 ployz deploy signs the automatic deploy rule
- **Surface:** CLI
- **Reach:** on an approved device, `ployz deploy` for a Git service whose rule doesn't match; with automatic deploys off; an older CLI's `ployz github rule approve`
- **Proof:** prints `Approving automatic deploys for production:` with namespace, repository, workflow, builder and servers, signed with the deploy's yes; with no rule possible it stops with `No automatic deploy rule covers web as it stands, so your servers won't build it. Turn on Deploy every push, then deploy again.`; the old command prints `ployz github rule approve is gone. Turn on Deploy every push in Ployz Cloud.`
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-server --cloud; verify-approval-binding
- **Compat:** CLI→Cloud
- **Docs:** deploy/github.md#approve-a-rule-for-deploys-on-push

### DEP-55 End the automatic deploy rule from the CLI
- **Surface:** CLI
- **Reach:** `ployz github rule end --project shop --env production` on an approved device
- **Proof:** the next push's build is refused on every Server; the dashboard switch reads off; it doesn't turn off the image rule (DEP-24)
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-server --cloud
- **Compat:** CLI→daemon
- **Docs:** deploy/github.md#approve-a-rule-for-deploys-on-push

### DEP-56 Wait for CI before a push deploys
- **Surface:** dashboard
- **Reach:** **Settings → Source → Wait for CI** on; push; send a `check_suite` `completed` event with `conclusion` `failure`, then `success`
- **Proof:** the push shows in **Runs** and `ployz deployment ls` as **Waiting for CI** named by commit; a failure reads **CI failed**; passing takes the next deployment number and deploys; a newer push replaces it; a cancelled one stays listed
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** deploy/github.md#wait-for-ci

### DEP-57 Deploy only when watched paths change
- **Surface:** dashboard
- **Reach:** **Settings → Source → Watch paths** → `apps/web/**` → **Add**; push a commit touching only `README.md`, then one touching `apps/web/x`
- **Proof:** the first push starts no build for the service; the second does; a force push or a 300-file push deploys every service on the branch
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1, with fixture files)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** deploy/github.md#deploy-only-when-certain-files-change

### DEP-58 Deploy one app of a monorepo
- **Surface:** dashboard
- **Reach:** two services from one repository with **Root directory** `/apps/web` and `/apps/worker` under **Settings → Source**
- **Proof:** each build's context holds only its folder (`ployz exec web -- ls /app` shows no `apps/worker`); a Dockerfile path resolves relative to the root directory; a push touching only `apps/worker/**` builds only `worker`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** deploy/github.md#deploy-a-monorepo
