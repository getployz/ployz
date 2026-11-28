# Services

What a user does with one Service and what its Servers do with it on their own: adding, renaming and deleting Services, every Service setting (source, networking, scale, deploy behavior), variables and sealed secrets, Configs, database presets, replicas and regions, and runtime control (restart, stop, start, `exec`, `ps`, `port-forward`). Environments, Branches and Sync are in [environments.md](environments.md); Deploy itself, history and zero-downtime in [deploy.md](deploy.md); domains in [networking.md](networking.md); Volumes in [volumes.md](volumes.md).

Start with `verify-cluster up --daemon <release>` (Standalone) for CLI and daemon entries, and verify-dashboard `SERVERS=1` (or `SERVERS=3` for regions) for dashboard ones. On a verify run, `ployz …` below means `scripts/verify-cluster cli "$run" -- …`; add `--device` for `exec` and `port-forward` on a `--cloud` run, and run `docker inspect` on a Machine with `scripts/verify-cluster exec "$run" machine-1 sudo docker …`. Dashboard handles are in `.agents/skills/verify-dashboard/features/canvas-and-inspector.md`. Staged changes reach the Servers only through a Deploy: dashboard **Deploy** asks for a passkey (deploy/deployments.md#deploy-from-the-dashboard), so when the browser can't sign, deploy with `ployz deploy` from the approved device instead. scaling.md and regions.md still say the Servers refuse a dashboard Deploy outright, which contradicts deployments.md: report it as a docs finding.

### SVC-01 Add a service from an image, a repository or nothing
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production` → **Create** → **Docker image** (`nginx:1.27`), **GitHub repository** or **Empty service**; also CLI `ployz service add cache --image nginx:1.27`, `ployz service add site --repo acme/web@main`, `ployz service add blank`; MCP `service_add`
- **Proof:** the canvas shows `link "cache, New, …"` before Deploy; `ployz service ls` lists `cache` as added by the next Deploy; after `ployz deploy`, `ployz ps` shows a `cache` container running. `ployz service add cache …` again is refused as taken
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: GitHub for the repository source (FAKE_GITHUB=1 covers it)
- **Drive:** verify-dashboard (SERVERS=1 for the running container)
- **Compat:** CLI→Cloud: a 0.3.19 CLI's `service add` writes a subset of what main's Config Store accepts
- **Docs:** services/services.md#add-a-service

### SVC-02 Find your way around a service on the canvas
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production` → click `web` → tabs **Deployments**, **Variables**, **Logs**, **Monitoring**, **Settings**; **Find** (`/`) → `dialog "Find a resource"`
- **Proof:** the inspector opens on **Settings** with the sections the source allows (Build only for a repository service, Storage only for a database without a volume); the URL gains `tab=` once a tab is picked; Find lists services, volumes and configs
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 for Logs and Monitoring data)
- **Compat:** none
- **Docs:** services/services.md#find-your-way-around-a-service

### SVC-03 List services and what the next deploy does to them
- **Surface:** CLI
- **Reach:** `ployz service ls`, `ployz service ls --json`, `ployz service ls | cut -f1`
- **Proof:** one row per service with what the next Deploy does (a staged new service reads as added, a removed one as removed); piped output is tab-separated with no color, so `cut -f1` prints only the names; an empty Environment prints one sentence
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud (or CLI→daemon Standalone): reads the Config Store; 0.3.19 output needs a cell
- **Docs:** **undocumented** (only --help; cli/overview.md names it in passing)

### SVC-04 Inspect one service
- **Surface:** CLI
- **Reach:** `ployz service inspect web`, `ployz service inspect web --json`; also MCP `service_inspect` (`{"service":"web"}`)
- **Proof:** prints the source, private DNS name, settings, variables, mounts, domains with DNS records still to create, staged changes and the Environment's revision; a sealed variable shows no value in either output
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: Config Store read; none beyond that
- **Docs:** **undocumented** (only --help)

### SVC-05 Rename a service and keep its private name
- **Surface:** CLI
- **Reach:** `ployz service rename web site` then `ployz deploy`; also dashboard: pencil next to the name in the inspector (`heading "Edit service name"`) → **Save**; MCP `service_rename`
- **Proof:** `ployz get worker.env.UPSTREAM` now reads `${{ site.… }}` for a reference that named `web`; after deploy, `ployz ps` lists `site`, and `ployz exec worker -- getent hosts web.internal` still resolves (the private name stays `web`); renaming to a taken name is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** services/services.md#rename-a-service

### SVC-06 Ployz refuses the reserved service names configs, volumes and setup
- **Surface:** CLI
- **Reach:** `ployz service add setup --image nginx:1.27`, `ployz service add configs --image nginx:1.27`; for an older Environment holding a `configs` service: `ployz service rename configs settings-service`
- **Proof:** each add is refused, naming the reserved name; the rename of a legacy `configs` service succeeds and keeps its private name, data and references
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** upgrade: an Environment made before `configs` was reserved may hold a service by that name
- **Docs:** services/services.md#rename-a-service

### SVC-07 Delete a service and see what it breaks
- **Surface:** CLI
- **Reach:** `ployz service rm redis`, `ployz diff`, `ployz deploy`; also dashboard: right-click the node or **Settings → Danger → Delete service**; MCP `service_remove`
- **Proof:** `service rm` prints each variable and config that references `redis`, with the command that changes it; `ployz diff` and `ployz status` show **Broken reference** like `redis was removed`; the service keeps running until the Deploy, after which `ployz ps` has no `redis` and the deploy warns that it left the referencing variable out. Discarding before Deploy brings it back. A never-saved **New** service is deleted at once after a confirm. MCP returns whether the deploy will ask approval
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** services/services.md#delete-a-service

### SVC-08 Deleting a database's service deletes its volume, after a confirm
- **Surface:** CLI
- **Reach:** `ployz service rm postgres` then `ployz deploy` (answer the `Go ahead with …? [y/N]`); also dashboard **Delete service** then **Deploy**
- **Proof:** the deploy lists `pg-data` and its data before asking; after yes, `zfs list` on the volume's Machine no longer has its dataset; when another service also mounts `pg-data`, the volume stays and nothing asks
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: destructive-deploy confirmation; a 0.3.19 CLI's confirm flow against 0.4 needs a cell
- **Docs:** deploy/deployments.md#confirm-a-deploy-that-deletes-data

### SVC-09 Anonymous volumes go only when a new container takes the old one's place
- **Surface:** daemon
- **Reach:** deploy a `postgres` image with no Ployz volume mounted at its data directory; change a variable and `ployz deploy` (a replace); then `ployz set db.replicas=…` lower, or `ployz service rm db` and deploy
- **Proof:** after the replace, `sudo docker volume ls --filter dangling=true` on the Machine does not grow; after the scale-down or removal, `ployz server volumes machine-1` lists the kept volume with its service, mount path and kept time; `ployz server volumes machine-1 prune --yes` deletes it
- **Needs:** 1 Machine · Volumes: Docker · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** runner→Server: a Server or a deploying CLI on 0.4.0-beta.2 or earlier keeps them on every removal; a beta.2 runner on a beta.3 Server leaves them on every replace (PLOY-12)
- **Docs:** services/services.md#delete-a-service

### SVC-10 Restart a service one container at a time
- **Surface:** CLI
- **Reach:** `ployz service restart web` (also `--signal SIGINT -t 5`); also dashboard: inspector **⋮** → **Restart**; MCP `service_restart`
- **Proof:** with 3 replicas on 3 Machines, `ployz ps` shows each container's start time change one after another while the others keep serving (dashboard header **Restarting 2/3**); a container that fails its healthcheck stops the restart there, the rest stay untouched, the command exits 3 and the card reads `Container N on machine-X didn't come back` with **Retry**; a staged change is not picked up
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone); local Cloud for the dashboard and MCP
- **Drive:** verify-server; verify-dashboard (SERVERS=3) for the header
- **Compat:** CLI→daemon: 0.4 changed the RPC catalog; a 0.3.19 CLI's restart against a 0.4 daemon needs a cell. Server upgrade: a container from before a 0.3→0.4 upgrade can't be started again until one signed deploy
- **Docs:** services/services.md#restart-stop-or-open-a-shell

### SVC-11 Stop a service until you start or deploy it
- **Surface:** CLI
- **Reach:** `ployz service stop web`, `ployz service stop web --signal SIGINT -t 5`; also dashboard **⋮** → **Stop** (asks first); MCP `service_stop`
- **Proof:** the service's domain stops answering from every Machine; `ployz ps` shows its containers exited and the service reads **Stopped** even when the app exits non-zero; `sudo docker ps -a` still lists the containers; with Ployz stopped on one Machine, the stop waits up to 5 minutes, then stops that container anyway
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: as SVC-10
- **Docs:** services/services.md#restart-stop-or-open-a-shell

### SVC-12 Start a stopped service
- **Surface:** CLI
- **Reach:** `ployz service start web`; also dashboard **Start** in the inspector header; MCP `service_start`
- **Proof:** `ployz ps` shows the same container IDs running again, and the domain answers once each passes its healthcheck; a container a failed deploy left behind runs but gets no requests, is named, and the command exits 3; a container a deploy replaced can't come back this way
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: as SVC-10; Server upgrade: a pre-upgrade container's start is refused (`This container was made before this server's upgrade ...`)
- **Docs:** services/services.md#restart-stop-or-open-a-shell

### SVC-13 Run a command or a shell in a service
- **Surface:** CLI
- **Reach:** `ployz exec web`, `ployz exec web -- sh -c 'exit 7'`, `ployz exec web -- nosuchprog`, `ployz exec -T web -- cat /etc/hostname`, `ployz exec --container <id-prefix> web -- true`, `ployz exec -d web -- sleep 60`
- **Proof:** the exit code passes through (`echo $?` → 7, and 127 for a missing program); with `-T`, the command's stderr stays off stdout; with a Machine powered off (`verify-cluster power off`) and no running container left, `ployz exec -T db -- true` names the missing Servers and exits 3
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`--device` on a `--cloud` run)
- **Compat:** CLI→daemon: exec runs on the Server; 0.3.19 CLI against 0.4 needs a cell
- **Docs:** services/services.md#restart-stop-or-open-a-shell

### SVC-14 List an environment's containers across servers
- **Surface:** CLI
- **Reach:** `ployz ps`, `ployz ps --sort machine`, `ployz ps --sort health`, `ployz ps --json`
- **Proof:** one row per container with its service, Server and state (`running`, `exited`, `restarting`, `draining`, `Stopped` for a stopped service); old pre-deploy containers are listed until turned off; a Server that didn't answer is named
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: reads each Server; 0.3.19 CLI against 0.4 needs a cell
- **Docs:** troubleshooting/deployments.md#my-service-shows-crashed-or-stopped

### SVC-15 Forward a local port to a service
- **Surface:** CLI
- **Reach:** `ployz service port-forward postgres 15432:5432` (or `0:5432` for a free port), then in another shell `ployz exec postgres -- printenv POSTGRES_PASSWORD`
- **Proof:** `psql -h 127.0.0.1 -p 15432 -U postgres ployz -c 'select 1'` returns 1 until Ctrl-C; with `0:5432` the command prints the port it picked
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server (`--device` on a `--cloud` run)
- **Compat:** CLI→daemon: the tunnel ends on the Server; needs a cell for 0.3.19
- **Docs:** services/databases.md#connect-from-your-laptop

### SVC-16 Read a service's settings from the CLI
- **Surface:** CLI
- **Reach:** `ployz get`, `ployz get web`, `ployz get web.memLimit`, `ployz get web.domains`, `ployz get --all`, `ployz get web --json`
- **Proof:** text prints quoted (`"nginx:1"`), unset as `-`, a named default as `No limit (default)` or `"rolling" (default Automatic)`, and `image` with `(no default)`; a value holding U+202E prints `\u{202e}`; `get web.domains` prints only domains
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: Config Store read
- **Docs:** services/settings.md

### SVC-17 Stage and unset settings from the CLI
- **Surface:** CLI
- **Reach:** `ployz set web.replicas=2 web.env.LOG_LEVEL=info`, `ployz set web --patch '{"healthcheck":{"path":"/up"}}'`, `ployz set web --patch -`, `ployz unset web.cpuLimit web.env.LOG_LEVEL`, `--expect <REVISION>` on either
- **Proof:** `ployz diff` lists each staged row; `--expect` with a stale revision is refused and changes nothing; one edit that sets and unsets `web.image` is refused; a change over 4 MiB fails with `invalid_argument`; `ployz discard web` drops it
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: a 0.3.19 CLI's `set` writes a field subset main's Config Store accepts
- **Docs:** services/settings.md

### SVC-18 Explain a setting and print the settings schema
- **Surface:** CLI
- **Reach:** `ployz explain web`, `ployz explain web.replicas`, `ployz explain mounts` (for a service named `mounts`), `ployz schema --json`, `ployz schema web.healthcheck`
- **Proof:** `explain web` lists every setting with its default; `explain mounts` also says `ployz explain mounts.mounts.data` describes the setting; `schema --json` has `$defs.service.properties` with keys like `cpuLimit`, `memLimit`, `restartPolicy`, and marks Cloud-only commands `"origin": "cloud"` (it prints with no Server reachable)
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md

### SVC-19 Switch a service between an image and a repository
- **Surface:** CLI
- **Reach:** `ployz set web.repository=acme/web`, `ployz set web.image=nginx:1.27`, `ployz set web.branch=dev web.repository=acme/web`; also dashboard: Source **⋯** → **Switch to container image** / **Switch to github repository**
- **Proof:** the first two stage the switch (`ployz diff`); `set web.image=nginx:1.27 web.buildMethod=dockerfile` and `set web.image=… web.repository=…` are refused; on an image service, setting `branch` with `repository` in the same `set` works whatever the order; a repository service refuses `unset web.branch`; after switching to an image, `ployz get web.rootDir` is back at its default
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: GitHub (FAKE_GITHUB=1 covers it)
- **Drive:** verify-dashboard (FAKE_GITHUB=1)
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** services/settings.md#switch-between-an-image-and-a-repository

### SVC-20 An empty service needs a source before it deploys
- **Surface:** dashboard
- **Reach:** **Create** → **Empty service** → **Deploy**; then **Settings → Source** → **GitHub repository** or **Container image**
- **Proof:** the first deploy fails for the empty service; after adding an image it deploys and `ployz ps` shows it running
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** none
- **Docs:** services/settings.md#add-a-source

### SVC-21 Deploy-on-push settings take effect at once
- **Surface:** dashboard
- **Reach:** repository service → **Settings → Source**: **Auto-deploy**, **Wait for CI**, **Watch paths**; **Build → Preferred Builder**; also `ployz set web.autoDeploy=false`, MCP `service_update`
- **Proof:** the change shows no staged row in **Details** or `ployz diff`, and a reload keeps it; MCP returns it under `immediate`; Discard does not undo Auto-deploy
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: GitHub (FAKE_GITHUB=1 covers it)
- **Drive:** verify-dashboard (FAKE_GITHUB=1)
- **Compat:** none
- **Docs:** services/settings.md#auto-deploy

### SVC-22 Pull a private image with sealed registry credentials
- **Surface:** CLI
- **Reach:** `printf %s "$TOKEN" | ployz set web.registryCredential --secret --username octo --registry ghcr.io`; also dashboard **Settings → Source → Add credentials**
- **Proof:** after `ployz deploy`, `ployz ps` shows the private image running; a token given inline (`--patch` with a new secret, or `'web.registryCredential={"secret":"…"}'`) is refused before anything is sent; without a terminal and without a pipe it exits 2 and prints the `printf … |` command; a repository service refuses a registry credential
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone) · External: a private registry image and token
- **Drive:** verify-server
- **Compat:** CLI→Cloud: main's Cloud refuses a plaintext registry login from a 0.3.19 CLI and tells it to update (migration 0055)
- **Docs:** services/settings.md#registry-credentials

### SVC-23 Change a service's private address
- **Surface:** CLI
- **Reach:** `ployz set web.privateDns=site` then `ployz deploy`; also dashboard **Settings → Networking**, textbox `Private address` + Enter
- **Proof:** `ployz exec worker -- getent hosts site.internal` resolves to web's replica, and `web.internal` no longer does; `ployz unset web.privateDns` returns it to the service's name
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md#private-address

### SVC-24 Run more replicas, one per server first
- **Surface:** CLI
- **Reach:** `ployz set web.replicas=3` then `ployz deploy`; also dashboard **Settings → Scale**, `One more`; MCP `service_update`
- **Proof:** on 3 Machines, `ployz ps --sort machine` shows one `web` container per Machine; `replicas=51` is refused (max 50); a service with a Volume shows **Limited to 1** and keeps one replica; a new Machine gets a replica only after the next `ployz deploy`
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-region-placement for spread
- **Compat:** none
- **Docs:** services/scaling.md#run-more-replicas

### SVC-25 Servers apply CPU and memory limits to each replica
- **Surface:** daemon
- **Reach:** `ployz set web.cpuLimit=0.5 web.memLimit=512MiB` then `ployz deploy`; also dashboard **Scale → Resource limits**
- **Proof:** `sudo docker inspect -f '{{.HostConfig.NanoCpus}} {{.HostConfig.Memory}}' <container>` prints `500000000 536870912`; `ployz diff` showed `0.5 vCPUs` and `512 MiB`; `ployz set web.memLimit=1073741824` is refused (plain numbers are GB); a replica that allocates past its limit is OOM-killed and its restart policy decides what follows
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md#memory-limit

### SVC-26 A limit change applies in place without a restart
- **Surface:** daemon
- **Reach:** on a running service: `ployz set web.cpuLimit=1` or raise `web.memLimit`, deploy; then lower `web.memLimit`, deploy
- **Proof:** after the first deploy, `ployz ps` shows the same container IDs and `docker inspect` shows the new limit; lowering memory (or a first memory limit, or any other change) replaces the container; if a Server fails to apply it, the deploy fails there and the replica keeps its old limit
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** runner→Server: when any Server running a replica runs an older Ployz, every replica replaces instead
- **Docs:** services/settings.md#changing-a-limit

### SVC-27 Override the start command
- **Surface:** CLI
- **Reach:** `ployz set web.startCommand='sh -c "echo started on $PORT; exec nginx -g \"daemon off;\""'` then `ployz deploy`; also dashboard **Settings → Deploy**, textbox `Start command`
- **Proof:** on an image service, `docker inspect -f '{{.Config.Entrypoint}}'` shows the command replacing the image's ENTRYPOINT, with no shell unless wrapped; `ployz logs web` shows `started on 8080`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md#start-command

### SVC-28 Run a pre-deploy command before each deploy
- **Surface:** CLI
- **Reach:** `ployz set web.preDeployCommand='sh -c "exit 1"'` then `ployz deploy`; then a passing command; then `ployz unset web.preDeployCommand` and deploy
- **Proof:** the failing command fails the deploy and the old version keeps answering on the domain; a passing one lets the new replica start; after turning it off, `ployz ps` stops listing the old pre-deploy containers
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md#pre-deploy-command

### SVC-29 A healthcheck gates a new replica's traffic
- **Surface:** daemon
- **Reach:** `ployz set web.healthcheck=/up`, `ployz set web --patch '{"healthcheck":{"command":"pg_isready -h 127.0.0.1","timeoutSeconds":60}}'`, then `ployz deploy`
- **Proof:** with `/up` failing, the deploy fails after `timeoutSeconds`, the new replica is stopped and `curl` on the domain still answers from the old version; with it passing, the old replica stops only afterwards; a service whose variables reference this one starts after it passes; a replica that passed and later fails keeps traffic, `ployz status` names it and its card shows an amber **⚠**; with no healthcheck, an image `HEALTHCHECK` is ignored
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-dashboard (SERVERS=1) for the ⚠
- **Compat:** none
- **Docs:** services/settings.md#healthcheck

### SVC-30 Servers restart a crashed replica by its restart policy
- **Surface:** daemon
- **Reach:** a service whose start command exits 1 after a few seconds: `ployz set worker.restartPolicy=on-failure worker.maxRetries=2`, deploy; compare `unless-stopped` (default), `always`, `no`
- **Proof:** `sudo docker inspect -f '{{.HostConfig.RestartPolicy.Name}} {{.HostConfig.RestartPolicy.MaximumRetryCount}} {{.RestartCount}}'` prints `on-failure 2` and settles at a restart count of 2 with the container exited; MCP `service_status` shows the restarts; with `unless-stopped`, the replica comes back after `sudo systemctl restart docker` but stays down after `ployz service stop`; `maxRetries=101` is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md#restart-policy

### SVC-31 Choose how a deploy replaces replicas
- **Surface:** CLI
- **Reach:** `ployz set web.updateOrder=rolling` (or `start-first`, `stop-first`), then change the image and `ployz deploy`; also dashboard combobox `Update order`
- **Proof:** with `rolling` on 2 Machines, `ployz ps` during the deploy shows one Machine's replica replaced before the other's; with `stop-first`, never two copies at once; `ployz get web.updateOrder` unset reads `(default Automatic)`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md#update-order

### SVC-32 Set a stop signal and grace period
- **Surface:** CLI
- **Reach:** `ployz set worker.stopSignal=SIGTERM worker.stopGracePeriod=120` then `ployz deploy`
- **Proof:** `docker inspect -f '{{.Config.StopSignal}} {{.Config.StopTimeout}}'` prints `SIGTERM 120`; a replica made before the change keeps the image's signal and 10 s until replaced; `stopGracePeriod=601` is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/settings.md#stop-signal

### SVC-33 Add, change or remove a variable
- **Surface:** dashboard
- **Reach:** `web` → **Variables** → **New Variable** (Key `log_level`, Value `info`) → **Add** → **Deploy**; row menu `Actions for LOG_LEVEL` to change or remove; also CLI `ployz set web.env.LOG_LEVEL=info`, `ployz unset web.env.LOG_LEVEL`
- **Proof:** the key is saved uppercase (`LOG_LEVEL`); a key starting with a digit is refused; the running service keeps the old value until deploy, then `ployz exec web -- printenv LOG_LEVEL` prints `info`; a repository service rebuilds on that deploy
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** services/variables.md#add-a-variable

### SVC-34 Paste many variables at once
- **Surface:** dashboard
- **Reach:** **Variables** → **Raw editor** (ENV or JSON) → edit → tick **Seal** under a new key → **Update variables**; also CLI `ployz set web --from-env-file .env` (add `--secret` to seal every value)
- **Proof:** a deleted line deletes that variable; sealed variables stay; a ticked new key arrives sealed (lock, `button "Sealed"`); with `--from-env-file` over 2 MiB of plain values the set lands with a note naming the total
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 for Seal)
- **Compat:** none
- **Docs:** services/variables.md#paste-many-at-once

### SVC-35 Rename a variable and its references
- **Surface:** CLI
- **Reach:** `ployz set web.env.API_URL --rename PUBLIC_API_URL`, then with `--update-references`; also dashboard row menu → **Rename** ("Also updates 2 references"); `ployz mcp` and the Agent rename with references
- **Proof:** without `--update-references`, a referenced variable is refused, listing its users and the command to rerun; with it, `ployz get worker.env.X` reads `${{ web.PUBLIC_API_URL }}`; renaming onto an existing name is refused and leaves both; a sealed variable stays sealed; a Config file that references it still blocks the rename
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** services/variables.md#rename-a-variable

### SVC-36 Seal a secret from the CLI
- **Surface:** CLI
- **Reach:** `ployz device pin`, then `printf %s "$VALUE" | ployz set web.env.TOKEN --secret`; at a terminal, `ployz set web.env.TOKEN --secret` prompts `Value for web.env.TOKEN:`
- **Proof:** `ployz get web.env.TOKEN` shows it sealed with no value; after deploy `ployz exec web -- printenv TOKEN` prints it; `ployz set 'web.env.TOKEN={"secret":"x"}'` is refused and points at `--secret`; with `--json` or `CI=1` and no pipe it exits 2 and prints the `printf %s … |` command; a multi-line paste at the prompt is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: an older `ployz` can't set a secret and says to update; Server: every Server needs 0.3.4 or later to open sealed values
- **Docs:** services/variables.md#seal-a-secret

### SVC-37 Seal a secret in the dashboard
- **Surface:** dashboard
- **Reach:** **Variables** → **New Variable** → tick **Sealed** → **Add**; on a plain row `Actions for KEY` → **Seal**
- **Proof:** the row shows `button "Sealed"` with no Show or Copy after reload; setting it back to a plain value is refused; there is no unseal; History shows its earlier versions as **Sealed**; with no Server in the Organization the form says why and keeps what was typed
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1; without it the seed's key is throwaway)
- **Compat:** none
- **Docs:** services/variables.md#seal-a-secret

### SVC-38 Sealed values open only on the servers
- **Surface:** daemon
- **Reach:** seal `web.env.TOKEN` (SVC-36), deploy; then Undo the version in History, or roll back to before it
- **Proof:** `ployz get web --json` and MCP `service_inspect` carry no plaintext; MCP `service_logs` replaces the value with its name or `[redacted]`; only `ployz exec web -- printenv TOKEN` shows it; after Undo or rollback it reads **Not set** and the deploy leaves it out with a warning until **Set value**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard (SERVERS=1)
- **Compat:** Server↔Server: a Server below 0.3.4 joins but can't start a service with sealed values
- **Docs:** services/variables.md#seal-a-secret

### SVC-39 A seal hides the earlier plain value in branches below
- **Surface:** dashboard
- **Reach:** production `TOKEN=plain-a`; branch `qa` from production (and `qa-two` from `qa`); in production **Seal** `TOKEN` and deploy; open `qa`'s **Details**, History, and `ployz diff --env qa`
- **Proof:** `qa`'s History and diff show the earlier value as **Sealed**, never `plain-a`; no **Use theirs** or `ployz env sync --take` hint offers it; discarding back to it brings `TOKEN` back **Not set**; a branch that never changed it gets `TOKEN` **Not set** as a change when the seal deploys
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** upgrade: a seal made before 0.3.15 may have left the plain value for a branch of a branch until it's saved again
- **Docs:** services/variables.md#limits-of-a-seal-in-branches-below

### SVC-40 Generate a random value
- **Surface:** CLI
- **Reach:** `ployz set web.env.API_KEY='${{ secret(48) }}' web.env.PIN='${{ secret(8, "0-9") }}' web.env.LIT='$${{ secret() }}'`
- **Proof:** `ployz get web.env.API_KEY` shows a 48-character plain value that stays through deploys, Discard and writing `${{ secret(48) }}` again; `PIN` is 8 digits; `LIT` holds the text `${{ secret() }}`; `secret(7)` and a character set with a space or quote are refused; a separate copy in a branch gets a different value
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** upgrade: a value saved before 0.3.6 as `${{ secret(32) }}` stays that text
- **Docs:** services/variables.md#generate-a-value

### SVC-41 Reference another service's variable
- **Surface:** CLI
- **Reach:** `ployz set web.env.DATABASE_URL='${{ postgres.DATABASE_URL }}' web.env.SELF='${{ PLOYZ_PRIVATE_DOMAIN }}'` then `ployz deploy`; also dashboard New Variable, type `${{` for suggestions (tick **Exported** to list a variable)
- **Proof:** `ployz exec web -- printenv DATABASE_URL` points at `postgres.internal`; the deploy starts `postgres` before `web`; in a branch with its own `postgres` the same reference reads the branch's; a reference to a sealed variable resolves in the container while `get` shows the reference text
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/variables.md#reference-another-services-variable

### SVC-42 A broken reference is flagged and left out of deploys
- **Surface:** CLI
- **Reach:** `ployz set web.env.X='${{ redis.NOPE }}'`, or reference `redis` and then `ployz service rm redis`; `ployz diff`, `ployz status`, `ployz deploy`
- **Proof:** the set is accepted; `diff`, `status` and the dashboard show **Broken reference** in red; the deploy leaves `X` and any variable that references it out and warns; a deleted service shows as `${{ <deleted redis>.KEY }}` (a second one as `<deleted redis#2>`)
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/variables.md#reference-another-services-variable

### SVC-43 Variables Ployz provides
- **Surface:** daemon
- **Reach:** deploy `web`, then `ployz exec web -- printenv PORT PLOYZ_MACHINE_ID PLOYZ_SERVICE_NAME`; also dashboard **Variables** → "N Ployz reference defaults"
- **Proof:** `PORT` is `8080`; `PLOYZ_MACHINE_ID` equals the Machine's ID in `ployz server ls --json` and overrides a user variable of that name; `PLOYZ_SERVICE_NAME` is empty until referenced (`SELF=${{ PLOYZ_SERVICE_NAME }}` prints `web`); a reference to `PLOYZ_MACHINE_ID` is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/variables.md#variables-ployz-provides

### SVC-44 Ployz refuses variables Linux can't start
- **Surface:** CLI
- **Reach:** `ployz set web.env.BIG="$(head -c 140000 /dev/zero | tr '\0' a)"`; `ployz set web --from-env-file /dev/zero`; a set totalling over 2 MiB, then over 6 MiB
- **Proof:** the 128 KiB+ value is refused at set; `/dev/zero` fails at once with `invalid_argument`; over 2 MiB lands with a note naming the total; over 6 MiB is refused; a deploy whose references or secrets resolve past a limit fails before anything runs, naming the variable
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/variables.md#good-to-know

### SVC-45 Create a config and write its files
- **Surface:** dashboard
- **Reach:** **Create** → **Config** → Name `App Settings` → **Create config** → **Add file** `conf.d/app.conf` → type → **Save**; also MCP `config_write` (creates the Config, optionally mounts it)
- **Proof:** the name field shows `app-settings`, the name it saves; **Details** lists the file as staged; a reload keeps it; a path with `..` or five segments is refused; unsaved edits stay visible after a failed save
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** services/configs.md#create-a-config

### SVC-46 Manage configs from the CLI
- **Surface:** CLI
- **Reach:** `ployz config add app-settings --mount web:/etc/app`, `ployz config ls`, `ployz config inspect app-settings`, `ployz config mount worker app-settings /etc/app`, `ployz config unmount worker app-settings`, `ployz config rename app-settings app`, `ployz config rm-file app app.conf`, `ployz config rm app`; also MCP `config_rename`, `config_remove`
- **Proof:** `config ls` shows each Config and what the next Deploy does to it; `config inspect` prints file text and mounts; after deploy, `ployz exec web -- ls /etc/app` reflects each step and `ployz exec web -- cat /etc/app/app.conf` prints the file put with `config put` (default mode `444 0:0`); `config rm` unmounts it everywhere on the next Deploy
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud through Cloud (Config Store write); Standalone keeps the Config Store locally. The Deploy that ships it signs the change, so it asks a person's yes at a terminal (see README, Driving from an agent)
- **Docs:** **undocumented** (only --help; configs.md describes only `config put` and the dashboard)

### SVC-47 Write a config file with its permissions from the CLI
- **Surface:** CLI
- **Reach:** `ployz config put app-settings app.conf --from ./app.conf --mode 0440 --uid 1000 --gid 1000`; `ployz config put app-settings run.sh --executable < run.sh`; also dashboard file options → **Permissions…** / **Executable**
- **Proof:** after deploy, `ployz exec web -- stat -c '%a %u %g' /etc/app/app.conf` prints `440 1000 1000`, `run.sh` is `555 0 0`; `ployz exec web -- touch /etc/app/x` fails read-only; `--from /dev/zero` fails at once
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/configs.md#set-file-permissions

### SVC-48 Mount a config into services
- **Surface:** dashboard
- **Reach:** `sentry` config → **Mounts** → **Mount on a service** → Service `api`, Directory `/etc/sentry` → **Mount**; `Actions for web mount` → **Edit directory** / **Unmount**
- **Proof:** after deploy, `ployz exec api -- cat /etc/sentry/config.yml` prints the file; a directory under `/proc`, `/sys` or `/dev`, or one already holding a config or volume, is refused and the form keeps what was typed; **Unmount** keeps the Config and its files
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** none
- **Docs:** services/configs.md#mount-it-into-a-service

### SVC-49 Reference variables inside a config file
- **Surface:** dashboard
- **Reach:** `sentry` → `config.yml` → type `${{ redis.REDIS_URL }}` and `${{ api.SECRET_KEY }}` → **Preview** → **Save** → Deploy
- **Proof:** **Preview** fills the plain reference and masks the sealed one; **Details** shows the authored `${{ … }}` text; in the container, `cat /etc/sentry/config.yml` holds the resolved values
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** none
- **Docs:** services/configs.md#reference-a-services-variable

### SVC-50 Review config changes; only mounting services restart
- **Surface:** dashboard
- **Reach:** edit `sentry/config.yml` → **Save** → bottom bar **Details** (Before/After, mode, uid, gid) → a file's **Discard** or **Discard all of sentry** → **Deploy changes**
- **Proof:** **Details** names `web` and `worker` as restarting; after deploy, `ployz ps` shows new container IDs for `web` and `worker` and the same ones for `api`; a redeploy with no change keeps all IDs; discarding a file restores both its text and permissions
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** none
- **Docs:** services/configs.md#review-or-discard-file-changes

### SVC-51 Ployz refuses configs too large to mount
- **Surface:** CLI
- **Reach:** `head -c 300000 /dev/urandom | base64 | ployz config put big a.txt`; three 250 KB files mounted on one service; a file whose references resolve past the limit
- **Proof:** the 300 KB file is refused (256 KB a file); the mount set past 768 KB is refused; the deploy refuses a config whose resolved variables push it past the limit
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/configs.md#good-to-know

### SVC-52 Tell apart a deleted config and its same-named replacement
- **Surface:** CLI
- **Reach:** `ployz config rm sentry`, `ployz config add sentry`, then `ployz diff --json`, `ployz discard configs.sentry`, `ployz discard configs.@<UUID>`, `ployz discard web.configs.@<UUID>`
- **Proof:** `diff --json` carries `web.configs.@UUID` beside `configName`; `discard configs.sentry` is refused as ambiguous; `configs.@UUID` discards exactly one, and discarding a mount keeps another config's mounts
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/configs.md#good-to-know

### SVC-53 Add a database with a sealed password
- **Surface:** dashboard
- **Reach:** **Create** → **Database** → **PostgreSQL** (also MySQL, Redis, MongoDB) → **Deploy**
- **Proof:** a `postgres` service with a 5 GB volume appears; its variables include `PGHOST`, `PGPASSWORD` and `DATABASE_URL`, with the password sealed (32 letters made in the browser); after deploy, `ployz exec postgres -- psql -U postgres ployz -c 'select version()'` reports 18; with no Server the menu reads **Add a server first**; the service has a command healthcheck
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** upgrade: a database added before 0.3.6, or before sealed passwords, keeps its plain password
- **Docs:** services/databases.md#add-a-database

### SVC-54 A database copied into a branch needs a new password
- **Surface:** CLI
- **Reach:** `ployz env branch staging --copy postgres`, then `ployz deploy --env staging`; then dashboard **Deploy** on staging
- **Proof:** the CLI deploy is refused with `postgres.POSTGRES_PASSWORD has no value yet. Deploy from the dashboard to make it.`; the dashboard Deploy makes and seals a new password and the copy starts on an empty volume; a branch that shares production's database leaves `DATABASE_URL` out of its deploys and the New branch panel names it
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** none
- **Docs:** services/databases.md#add-a-database

### SVC-55 Back up and restore a database from the CLI
- **Surface:** CLI
- **Reach:** `ployz exec -T postgres -- pg_dump --format=custom > postgres.dump`, then `ployz exec -T postgres -- pg_restore --clean --if-exists -d ployz < postgres.dump`; the MySQL, Redis and MongoDB lines from the page
- **Proof:** the dump file starts with `PGDMP`; a table dropped after the dump is back after the restore; the Redis dump is a valid RDB (`REDIS` magic)
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: as SVC-13
- **Docs:** services/databases.md#back-up-a-database

### SVC-56 A deploy leaves waiting a service whose volume's server takes no services
- **Surface:** CLI
- **Reach:** `ployz server set machine-1 --accepts-services=false` where `pg-data` lives, then `ployz deploy`
- **Proof:** the deploy reports `postgres: Volume pg-data is on machine-1, which doesn't run services. Turn them back on with ployz server set machine-1 --accepts-services=true`, deploys the rest, and the deployment reads **Deployed, 1 waiting**
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/scaling.md#give-servers-different-jobs

### SVC-57 Bring services back when a server goes down
- **Surface:** CLI
- **Reach:** `verify-cluster power off "$run" machine-2`, then `ployz deploy` (or `ployz deploy web worker`); later `power on`
- **Proof:** stateless services start on machine-1 and the deploy exits 3 naming machine-2; a service whose volume is on machine-2 and its dependents read **Not attempted**, and the deployment **Deployed, N waiting**; `ployz ps` and `ployz logs` name machine-2 as not answering; after power on, machine-2's replaced replicas leave routing within minutes but keep running until the next deploy
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server↔Server: behavior differs before 0.3.8, 0.3.10, 0.3.12 and 0.3.14 (skipping silent or stopped Servers, removing stale replicas); a mixed cluster needs a cell
- **Docs:** services/scaling.md#when-a-server-goes-down

### SVC-58 Run a service in more than one region
- **Surface:** CLI
- **Reach:** label Machines `region=fsn` ×2 and `region=syd`; `ployz set api.replicas='{"fsn":2,"syd":1}'`, `ployz deploy --plan`, `ployz deploy`; then `'{"fsn":1,"syd":1}'`; `ployz set api.replicas=3`; also dashboard **Scale** → **Add region** / **×**
- **Proof:** `--plan` prints `api → fsn 2 · syd 1`; `ployz ps --sort machine` shows two replicas on fsn Machines and one on syd; lowering fsn removes one fsn replica and leaves the other two IDs; `'{"lhr":1}'` is refused with `No Server is in lhr. Regions: fsn, syd.`; **Details** reads `fsn 2 → fsn 2 · syd 1`; **Add region** is hidden for a service with a volume
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone); local Cloud for the dashboard
- **Drive:** verify-server; verify-region-placement
- **Compat:** CLI→Cloud: an older `ployz` shows and sets per-region counts as `{"fsn":2,"syd":1}` but not the Environment's region
- **Docs:** services/regions.md#run-a-service-in-more-than-one-region

### SVC-59 A service whose volume is outside the region waits
- **Surface:** CLI
- **Reach:** `postgres` with `pg-data` on an fsn Machine; `ployz env region syd`, `ployz deploy`; then `ployz volume move pg-data --to <syd machine>` or `ployz set postgres.replicas='{"fsn":1}'`
- **Proof:** the deploy reports `postgres: not started (its Volume pg-data is on …)` and prints both ways out; after either, the next deploy starts `postgres`; with the volume's Machine in no region, the line names the Machine and suggests `ployz server set <m> --label-add region=syd`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server; verify-region-placement
- **Compat:** none
- **Docs:** services/regions.md#when-a-volume-is-outside-the-region

### SVC-60 Replicas wait while their region has no server
- **Surface:** CLI
- **Reach:** `ployz set api.replicas='{"fsn":2,"syd":1}'` with no syd Machine; `ployz deploy --plan`, `ployz deploy`; then `ployz server set <syd-1> --accepts-services=false` with a syd replica running and change the image
- **Proof:** `--plan` prints `api → fsn 2 · syd 1: 1 replica waits (no Server in syd)`; the deploy prints `1 deployed · 1 replica waiting`, reads **Deployed, 1 waiting** and exits 3; with every count in empty regions, `web: not started (no Server in fsn)` and **Nothing deployed, N waiting**; the syd replica stays on its old version (`1 replica on the old version`); a spare replica keeps running elsewhere until syd starts its own
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-region-placement
- **Compat:** none
- **Docs:** services/regions.md#when-no-server-is-in-the-region

### SVC-61 Services that share a volume must run in one region
- **Surface:** CLI
- **Reach:** `reader` mounting `pg-data` with `postgres`; `ployz set reader.replicas='{"syd":1}'` while pg-data is in fsn; `ployz deploy`, `ployz deploy reader`; also dashboard **Scale** and **Mount on a service** on reader
- **Proof:** the deploy starts `postgres` and reports `reader: not started (reader and postgres share the Volume pg-data, … Set reader to fsn)`, reading **Failed**; deploying `reader` by name is refused the same way; the dashboard offers reader only fsn with `Must run in fsn. It shares the Volume pg-data with postgres, …`; counts in two regions for a service with a volume are refused
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone); local Cloud for the dashboard
- **Drive:** verify-server; verify-region-placement
- **Compat:** none
- **Docs:** services/regions.md#good-to-know

### SVC-62 Global services catch up on a new server
- **Surface:** daemon
- **Reach:** a running Cluster; `scripts/verify-cluster add "$run"` (or `ployz server add …`)
- **Proof:** the join ends with no Global catch-up error; on the new Machine `sudo docker ps --format '{{.Names}}'` lists the Ingress Proxy (and the Private Access router when on); a generated domain answers through the new Machine's address; when the joining `ployz` is older than another Server it ends with `Server joined, but Global catch-up is incomplete; it remains a Cluster member.` and the Server stays a member
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server↔Server: a 0.3.x Server can't join a 0.4 Cluster (#4194); a 0.3.3-or-older `ployz` can't read sealed-variable services and leaves catch-up incomplete
- **Docs:** troubleshooting/servers.md#a-new-server-joined-but-global-catch-up-cant-read-another-server

### SVC-63 A server starts a container only from a deploy a trusted key still signs
- **Surface:** daemon
- **Reach:** deploy `web` from device B; from device A `ployz device rm <B>`; then `ployz service stop web` and `ployz service start web`, or the dashboard **Start**; finally `ployz deploy` from A
- **Proof:** the start of the container only B admitted is refused; a new signed deploy from A starts a fresh copy that `ployz ps` shows running; after a 0.3→0.4 upgrade, starting a pre-upgrade container is refused with `This container was made before this server's upgrade ...` and a stop-first deploy that fails reads `Could not restore the old Container.`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-service-floor
- **Compat:** Server upgrade: after 0.3→0.4 every Service needs one signed deploy before its containers can be started again (#4557, #4458)
- **Docs:** deploy/deployments.md#after-a-server-upgrade-deploy-each-service-once
