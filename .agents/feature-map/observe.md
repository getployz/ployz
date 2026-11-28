# Observe (OBS)

Seeing what runs: live and stored logs (the Environment's **Logs** page, a service's **Logs** tab, deployment logs, `ployz logs`, `ployz server logs`), the Collector every Server connected to Cloud runs to ship logs, metrics and request rows to Cloud's OTLP routes with Sealed values masked, metrics (charts, canvas badges, `ployz metrics`), uptime checks, web analytics, the crash and uptime emails, and the public trace page. Verifiers start with `verify-cluster up` for anything read live from Servers, and need a Cloud with ClickHouse for anything read from history (search, removed services' logs, metrics charts with real readings, analytics). Neither verify-dashboard nor the soak self-host setup starts ClickHouse today: add `--profile observe` and `CLICKHOUSE_PASSWORD` (self-hosting.md) or set `OBSERVE_CLICKHOUSE_URL` in `.verify/run/env`. Without it, verify-dashboard's metrics fixture and uptime seed prove the charts and states render, not that a Server ships. The daemon's OTLP pushes land on `/api/machines/otlp/v1/{logs,metrics,traces}`; analytics salts on `/api/machines/<id>/analytics-salts`.

### OBS-01 Watch an Environment's logs live
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/logs`; narrow with **All services** / **All servers**; toggle **Error**, **Warn**, **Info**, **Debug**; scroll up, then **Latest**
- **Proof:** a line written by `ployz exec web -- sh -c 'echo hello-obs >/proc/1/fd/1'` appears without reload; level filters hide lines (a JSON `"level":"error"` line shows under **Error**, a bare stderr line under **Info**); while scrolled up new lines wait and **Latest** shows them; lines name service, server and short container id when a deploy replaced a container
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: live lines stream from Servers; a Server on an older ployz shows **Upgrade web-1 to see its logs**
- **Docs:** observe/logs.md#see-your-logs

### OBS-02 Search stored logs
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/logs` → **Search logs** `hello-obs`; clear the search
- **Proof:** matches from every server the Collector shipped, in time order with newest at the bottom, including lines older than the page holds; clearing returns to live; without ClickHouse it filters only loaded lines
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse (`observe` profile)
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: 0.3.19 Servers run no Collector, so main Cloud has no history for them
- **Docs:** observe/logs.md#see-your-logs

### OBS-03 A service's Logs tab
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production` → click web → **Logs**
- **Proof:** shows only web's lines; on a Cloud without ClickHouse it reads **Log history isn't set up.**; a container started before 0.4 reads **This service isn't sending logs yet.** with **Redeploy**; a quiet server reads **A server hasn't sent logs for 5 minutes.** linking the server
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 (DAEMON=0.3.19 then upgrade for the old-container case)
- **Compat:** upgrade: containers created by 0.3.19 keep Docker's `local` driver until redeployed
- **Docs:** observe/logs.md#when-logs-are-off

### OBS-04 A deployment's build and container logs
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/deployments/<id>` → the service's build card; then its service card; also **Logs** in the bottom bar while it runs
- **Proof:** the build card shows the build output (none for an image service); the service card shows what the new containers printed, including the pre-deploy command, still after a later deploy replaced them
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** observe/logs.md#see-a-deployments-logs

### OBS-05 Read logs from the CLI, straight from the Servers
- **Surface:** CLI
- **Reach:** `ployz logs web`, `ployz logs web --tail 50`, `ployz logs web 2>/dev/null`, `ployz --json logs web`, `ployz logs web:<container-prefix>`, `ployz logs --since 10m --until 2m --utc`
- **Proof:** a Sealed value prints as `[secret NAME]`; `--tail 50` prints 50 lines total, not per replica; nginx's stderr lines vanish with `2>/dev/null`, and `--json` puts every line on stdout with `stream`; a failed replica Docker still has reads in full; with a Server partitioned it's named and the exit code is 3
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`partition on` for the partial case)
- **Compat:** CLI→daemon: a 0.3.19 CLI's history route (`logs.history.v2`) is gone on 0.4 daemons
- **Docs:** observe/logs.md#read-logs-from-the-cli

### OBS-06 Follow logs across a redeploy and a lost server
- **Surface:** CLI
- **Reach:** `ployz logs web --follow`, then `ployz deploy web`; `ployz logs --follow --machine machine-2`; restart machine-2's daemon; `verify-cluster partition on machine-2`
- **Proof:** the follow moves onto the new container within about 2 seconds; `--machine` with no service follows every service there; a daemon restart prints `machine-2 stopped answering; its lines are missing from here on.` at once, a partition within about 20 seconds; it exits 3 once none answers and doesn't pick the server back up; `--follow` refuses a service the Environment doesn't have before reading
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon
- **Docs:** observe/logs.md#read-logs-from-the-cli

### OBS-07 Logs of a removed service come from Cloud
- **Surface:** CLI
- **Reach:** deploy `oldsvc`, let it print, `ployz service rm oldsvc && ployz deploy`, then `ployz logs oldsvc`; also on a Standalone Cluster; and `ployz logs nosvc`
- **Proof:** signed in to Cloud it prints oldsvc's lines oldest first from history (30 days max), Sealed values as `[secret NAME]`; Standalone it says the logs aren't kept and exits 1; `nosvc` fails `No Service named nosvc in Environment production`, lists services, exits 1 (3 if other names already printed); the dashboard **Logs** page with oldsvc selected shows the same lines
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse
- **Drive:** verify-server (`--cloud`)
- **Compat:** none
- **Docs:** observe/logs.md#read-logs-from-the-cli

### OBS-08 Logs of one deployment and its build
- **Surface:** CLI
- **Reach:** `ployz logs --deployment 7`, `ployz logs --deployment 7 --build web`
- **Proof:** only containers deployment 7 created, running or not; `--build` prints the build log on stdout and matches the build card; a failed deploy's `inspect:` line names this command
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`--cloud`)
- **Compat:** none
- **Docs:** troubleshooting/builds.md (`--deployment`/`--build` appear there and in troubleshooting/deployments.md; observe/logs.md doesn't describe the flags)

### OBS-09 Members and tokens read logs through Cloud
- **Surface:** CLI
- **Reach:** with `linusToken` or a read token: `ployz logs web`, `ployz logs web --follow`, `ployz ps`, `ployz metrics web`; with a staging Deploy token: `ployz logs --env production`
- **Proof:** prints each server's newest 200 lines at most, all on stdout, Sealed values scrubbed; `--follow`, `--until`, `--machine` and a container name are refused; a silent server is named and exit 3; the staging token fails `This token can deploy only to staging.`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server
- **Docs:** cli/overview.md#manage-your-team

### OBS-10 Read a Server's daemon logs (`ployz server logs`)
- **Surface:** CLI
- **Reach:** `ployz server logs`, `ployz server logs docker corrosion -m machine-1 --tail 20`, `ployz server logs -f`; as Linus
- **Proof:** prints the ployz daemon's lines by default, `docker` and `corrosion` on request, per server; `-f` streams; a member gets `Server access needs an admin. Ask an owner or admin of ada.`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon
- **Docs:** **undocumented** (only --help; cli/overview.md#manage-your-team names it as needing an admin)

### OBS-11 Agents read logs (`service_logs`, `server_logs`)
- **Surface:** MCP
- **Reach:** `service_logs` for web; `server_logs` for machine-1 with `ployz` and `docker`; also via the Agent sidebar
- **Proof:** `service_logs` returns recent lines per server with Sealed and secret-shaped values replaced, no follow; `server_logs` returns at most 200 lines with secret-shaped values replaced; a support session can't read `server_logs`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`--cloud`)
- **Compat:** Cloud→Server
- **Docs:** account/agent-sidebar.md#what-it-can-do-on-your-servers

### OBS-12 The Collector ships every container's logs to Cloud
- **Surface:** daemon
- **Reach:** connect a Server to a Cloud with ClickHouse; `docker ps --filter name=ployz-collector`; `docker inspect web-… --format '{{.HostConfig.LogConfig}}'`; block Cloud for a while, write lines, unblock
- **Proof:** a `ployz-collector` container runs on Cloud-connected Servers and none on Standalone ones; app containers use `json-file` with `max-size 10m`, `max-file 3`; lines written during the outage arrive after (unless over 30 MB); on first connect it backfills what containers still have on disk; config and mask live under `/run/ployz/collector` (root-only) and go on reboot
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse
- **Drive:** verify-server (`--cloud`, `partition on`)
- **Compat:** upgrade: 0.3.19 Servers push to `/api/machines/<id>/observe`, which main Cloud no longer serves; after upgrading, old containers and the Ingress Proxy stay on `local` until redeployed (`server set --accepts-ingress=true` for the proxy)
- **Docs:** observe/logs.md#logs-sent-to-ployz-cloud (self-hosting.md#keep-logs-metrics-and-traces-in-cloud-optional still says servers don't send them yet)

### OBS-13 Sealed values are masked before lines leave the Server
- **Surface:** daemon
- **Reach:** a Sealed `web.env.API_KEY` the app prints whole, base64-encoded, inside a 20 KiB line, and a 4-byte Sealed `PIN` printed as `PIN=4821` and bare; seal a new variable and deploy
- **Proof:** Cloud history, the Logs page and `ployz logs` show `[secret API_KEY]` for the whole value; base64 and the 16 KiB-split copy show through (documented limits); `PIN=4821` is hidden, a bare `4821` isn't; the deploy adding a Sealed value stops the Collector first and the new container's first line is already masked; a multiline Sealed value with a 1–5 byte line refuses the deploy; lines it couldn't check show **Hidden: couldn't check these lines for Sealed values.**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse
- **Drive:** verify-server (`--cloud`)
- **Compat:** none
- **Docs:** observe/logs.md#secrets-in-logs

### OBS-14 The Logs page names servers it can't read
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/logs` with one Server partitioned, all Servers off, and one Server on an older ployz
- **Proof:** a banner names the silent server while others' lines show; all off reads **Your servers are offline** and keeps the last lines; a dropped stream reads **Couldn't load logs** and retries; an old server reads **Upgrade web-1 to see its logs** linking its page; a server failure reads **Container logs are unavailable.**
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2 (`DAEMON="checkout machine-2=0.3.19"` for the old-server case)
- **Compat:** Cloud→Server
- **Docs:** observe/logs.md#good-to-know

### OBS-15 A service's metrics charts
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production` → web → **Monitoring**; pick ranges 5 minutes to 30 days; **By server**
- **Proof:** **CPU** (100% = one core, dashed limit), **Memory**, **Network**, **Disk** on one time axis with a shared cursor; deploy rules and OOM/exit markers; **By server** draws one line per server; a managed-volume service ends with a **Managed volume** row; the postgres fixture's gap draws as **Not measured**, never zero
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: ClickHouse for real readings (the verify fixture covers rendering)
- **Drive:** verify-dashboard (fixture) and SERVERS=1 with ClickHouse
- **Compat:** upgrade: readings from before 0.4 are gone; 0.3.19 Servers send none to main Cloud
- **Docs:** observe/metrics.md#see-a-services-metrics

### OBS-16 Metrics from the CLI (`ployz metrics`)
- **Surface:** CLI
- **Reach:** Standalone `ployz metrics web`; signed in `ployz metrics web --since 6h`, `--since 2026-01-31`, `--json`; `ployz metrics nosvc`
- **Proof:** Standalone prints container count, CPU, memory and byte totals and ends **History is in Ployz Cloud.**; with Cloud one row per metric with **NOW**, **AVG**, **PEAK** and an OOM/exit count; without ClickHouse it prints live numbers and **Metrics history isn't enabled on this Cloud.**; `nosvc` fails with exit 1 listing services
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone), then local Cloud with ClickHouse
- **Drive:** verify-server
- **Compat:** CLI→daemon: `metrics.history.v1` is gone on 0.4 daemons, so a 0.3.19 CLI reads no history
- **Docs:** observe/metrics.md#read-metrics-from-the-cli

### OBS-17 A Server's own metrics and disk
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers/<id>`; `ployz metrics --server machine-1`
- **Proof:** CPU, memory, network per interface (lo and docker0 hidden) and disk split into volumes, mirrors, logs and metrics, other; managed volume usage updates each minute; a server silent for a minute reads **machine-1 isn't reporting.**; the CLI prints live numbers without Cloud
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: ClickHouse for real readings
- **Drive:** verify-dashboard (fixture server `0123…`) and SERVERS=1
- **Compat:** Cloud→Server
- **Docs:** observe/metrics.md#see-a-servers-metrics

### OBS-18 Agents read metrics (`service_metrics`, `server_metrics`)
- **Surface:** MCP
- **Reach:** `service_metrics` for postgres with a range; with a deployment number; `server_metrics` for machine-1; `deployment_show` on a failed deploy
- **Proof:** returns readings with each OOM kill and exit and its server; a deployment number starts the range at that deployment and keeps only its events; `deployment_show` names `service_logs` and `service_metrics` for the failed service; without Cloud history both say so
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse
- **Drive:** verify-server (`--cloud`)
- **Compat:** none
- **Docs:** observe/metrics.md#ask-an-agent

### OBS-19 Canvas badges for OOM and crash loops
- **Surface:** dashboard
- **Reach:** a service with a 64 MiB memory limit that allocates more; a service that exits 1 twice within 10 minutes; `ployz service restart web` on an app that exits 143
- **Proof:** the card shows **⚠ OOM** (amber, red when down), then **⚠ Restarting** until the replica stays up 10 minutes; restart/deploy exits show nothing; several issues read **⚠ OOM +1**; clicking ⚠ opens **Monitoring**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** observe/metrics.md#spot-trouble-on-the-canvas

### OBS-20 The Collector ships metrics every 10 seconds
- **Surface:** daemon
- **Reach:** a Cloud-connected Server; query ClickHouse or read the Monitoring tab; block Cloud for 10 minutes, unblock
- **Proof:** new readings land every 10 seconds per container and server; readings held during the outage arrive after, a long outage leaves **Not measured**; a Docker (non-managed) volume adds no disk usage row
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: ClickHouse
- **Drive:** verify-server (`--cloud`, `partition on`)
- **Compat:** Cloud→Server
- **Docs:** observe/metrics.md#good-to-know

### OBS-21 Empty metrics say why
- **Surface:** dashboard
- **Reach:** Monitoring on a Cloud without ClickHouse; on a Server stopped a minute ago; on a service deployed seconds ago
- **Proof:** **Metrics aren't enabled on this Service** (or **Server**); **web-1 isn't reporting.** with older data still drawn; **No readings.**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (fixture: server `4444…` has no readings, other ids aren't reporting)
- **Compat:** none
- **Docs:** observe/metrics.md#when-a-chart-is-empty

### OBS-22 A service's uptime on the Monitoring tab
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production` → web → **Monitoring**; click an incident; type **From**/**Until**
- **Proof:** seeded states: web **Up** with a percentage, 90 days and a closed incident; api **Unreachable** with a red **⚠ Unreachable** on its card; redis **Checking**; postgres has no public address so Monitoring starts at metrics; an incident shows what each check got back
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud (wait for `.verify/run/uptime` = `ok`)
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** observe/uptime.md#see-a-services-uptime

### OBS-23 Uptime checks right after a deploy, and on demand
- **Surface:** dashboard
- **Reach:** deploy a change that breaks web's address; click **Check now**
- **Proof:** web reads **Unreachable** within seconds of the deployment succeeding; a failed deployment isn't checked at once; **Check now** reads **Checking** until the result, which counts toward the percentage; between deploys a check every 5 minutes
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: checks run from outside the Servers, so the checker must reach a Boat Machine (IPv6) on a real name
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** observe/uptime.md#check-right-after-a-deploy

### OBS-24 Uptime from the CLI and agents
- **Surface:** CLI
- **Reach:** `ployz uptime show web`, `ployz uptime show web --project shop --env staging --from 6h`, `ployz uptime check web`; MCP `uptime_show`, `uptime_check`
- **Proof:** `show` prints status, pass share, each region's latest result and last error, incidents; `--from`/`--to` take `15m`, `7d` or a UTC instant; `check` prints each region's answer and says so if none in about 10 seconds; postgres answers `not_checked`; without Production `needs_production`; `PLOYZ_PROJECT` set to a non-linked project uses its default Environment
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (seeded uptime)
- **Compat:** CLI→Cloud: published command, needs no CLI upgrade
- **Docs:** observe/uptime.md#from-the-cli-or-an-agent

### OBS-25 A 4xx at the healthcheck path shows how to fix it
- **Surface:** dashboard
- **Reach:** worker → **Monitoring** (seeded 404 at `/health`); change **Healthcheck path**, deploy
- **Proof:** the tab shows the **Healthcheck path** field with the app's 404; after the change the next check uses the new path; at `/` a 404 still counts as up
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** observe/uptime.md#good-to-know

### OBS-26 Turn on web analytics
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/analytics` → **Turn on** as Ada, and as Linus
- **Proof:** before: **Analytics are off for this site** with **Turn on**; after: the page says no visits yet until the first lands, in the browser's time zone kept for good; Linus can't turn it on; without ClickHouse it names what to set up; with BILLING=1 and no Production, **Analytics is part of Production**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud (`ANALYTICS_ENABLED=true` or BILLING=1)
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** observe/analytics.md#turn-it-on

### OBS-27 Read visitors, pages and sources (Web)
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/analytics` → **Web**; **Today**, **7 days**, **30 days**, **From**/**Until**; click a page, **Page is**
- **Proof:** cards **Visitors**, **Pageviews**, **Views per visit**, **Bounce rate**, **Visit duration**; lists **Pages**, **Sources**, **Entry pages**, **Campaigns**, **Languages**, **Hosts**; clicking a page filters, **Page is** inverts; today is dashed; Googlebot rows add no visits
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: ClickHouse (seed with `scripts/verify/seed-requests.ts`)
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** observe/analytics.md#web

### OBS-28 Route speed and failures (API, Errors)
- **Surface:** dashboard
- **Reach:** **API**: sort by **p95**, pick **1h**…**30d** and a service; **Slowest**; **Errors**
- **Proof:** routes read `/users/:id`, not `/users/42`; **Errors** % is 5xx share; seeded slow `/reports/:id` tops **Slowest** with a request id; **Errors** charts 4xx/5xx per hour with **By route** and **Newest**; bots appear in neither
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: ClickHouse (seed-requests.ts)
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** observe/analytics.md#api

### OBS-29 Count a goal
- **Surface:** dashboard
- **Reach:** **Goals** → name `signup`, path `/signup*`
- **Proof:** counts visitors who reached `/signup` or `/signup/done`; **Rate** is goal visitors over all visitors in the range
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: ClickHouse (seed-requests.ts)
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** observe/analytics.md#goals

### OBS-30 Request rows leave the Server without the visitor's address
- **Surface:** daemon
- **Reach:** analytics on for production; `curl` web's domain from two addresses on two days; read `otel.ployz_request` in ClickHouse and the Server's `/api/machines/<id>/analytics-salts` fetches
- **Proof:** each row carries time, service, host, method, path, status, duration, size, request id, referrer host, language, UTM tags, user agent and a visitor hash; no IP column holds an address; the same visitor's hash differs across days (salt per site-day); an Environment with analytics off ships no rows
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: ClickHouse; a reachable domain (sslip.io on the Boat IPv6) or `curl --resolve`
- **Drive:** verify-server (`--cloud`)
- **Compat:** Cloud→Server: salts route is new in 0.4; 0.3.19 Servers ship no request rows to main Cloud
- **Docs:** observe/analytics.md#good-to-know

### OBS-31 Crashed-service emails
- **Surface:** daemon
- **Reach:** a service that exits 1 on start (or OOMs) on a Cloud-connected Server; let it crash several times; then `ployz service restart` it
- **Proof:** Mailpit gets one **Crashed services** email naming the service, the Server, the crash count and last exit code, at most one per hour with later crashes counted in; a stop, restart or deploy you asked for sends none; members with **None** get nothing
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud (MAIL=1) · External: Cloud log shipping (ClickHouse) on a self-hosted-mode Cloud
- **Drive:** verify-dashboard MAIL=1 SERVERS=1
- **Compat:** Cloud→Server: crash events ride the Collector's OTLP logs push, so 0.3.19 Servers send none
- **Docs:** account/notifications.md

### OBS-32 Uptime incident emails
- **Surface:** dashboard
- **Reach:** break web's address so a check fails, then fix it
- **Proof:** Mailpit gets a down email and a recovered email; with BILLING=1 and no Production none are sent but the incident is recorded; an email that can't go within an hour is skipped
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud (MAIL=1) · External: checker must reach the Server's address
- **Drive:** verify-dashboard MAIL=1 SERVERS=1
- **Compat:** none
- **Docs:** account/notifications.md

### OBS-33 See which Server answered (`/.well-known/ployz/trace`)
- **Surface:** daemon
- **Reach:** `curl http://<domain>/.well-known/ployz/trace`; `curl --resolve <domain>:80:<ip> …` per Server; `curl -sI https://<domain> | grep -i ployz`; stop web and repeat; a POST to the path
- **Proof:** `key=value` lines in order (`server`, `deploy`, `replicas`, `status`, `ip`, `http`, `tls`, `sni`, `request_id`, `ts`), always 200; `status=ok`, `NOT_RUNNING` after stop, `NOT_DEPLOYED` on an unknown host; every response carries `ployz-server` and `ployz-request-id` matching the page; POST reaches the app; no service names or versions show
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/trace.md#read-the-trace-page
