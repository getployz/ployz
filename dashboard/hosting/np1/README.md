# Ployz hosted stack on np1

Ployz Cloud (ployz.dev), its staging copy (staging.ployz.dev), and the hosted-dns API (dns.ployz.app) run on np1, a
Hetzner root server at 5.9.85.203 / 2a01:4f8:162:4cb::2. np1 is a single-machine Uncloud cluster (uncloudd 0.19), and
Uncloud's Caddy terminates TLS. np1 also runs other projects; these files touch only the `ployz-*` services, the
`ployz-*` volumes and the routes they publish.

| File | Purpose |
|---|---|
| `compose.prod.yaml` | web, worker, inngest, redis-inngest, site, site-rebuild, tools-api, uptime, uptime-agent-eu, hosted-dns |
| `compose.staging.yaml` | staging web, worker, inngest, redis-inngest, postgres |
| `hosted-dns.Dockerfile` | builds getployz/hosted-dns (Railway used Railpack) |
| `site-rebuild/` | the hourly site rebuild sidecar: `Dockerfile`, `rebuild.sh`, `crontab` |

The relay (relay.ployz.dev) has its own compose file and is not covered here.

## Layout on np1

Everything lives in `/root/ployz/`, next to these compose files. Env files are mode 0600, are never committed, and
use one file per service: `.env.<service>.production` and `.env.<service>.staging`. They hold names that match the
app's config (`dashboard/src/server/config.server.ts`, `dashboard/uptime/src/config.rs`). Staging Postgres and Redis
credentials are generated on np1 and exist nowhere else.

Data is stored on ZFS datasets with quotas, mounted through named local volumes:

| Dataset | Mount | Quota |
|---|---|---|
| `data/ployz-prod-redis-inngest` | prod Redis `/data` | 10G |
| `data/ployz-staging-postgres` | staging Postgres `/var/lib/postgresql` | 20G |
| `data/ployz-staging-redis-inngest` | staging Redis `/data` | 10G |

Prod application data is on PlanetScale: org `ployz`, database `ployz` (app tables, Inngest state in schema
`inngest`, the uptime control role), and tools-api's `tools_status` schema.

## Deploy

`uc` runs on np1 against the local daemon, so the env files never leave the machine:

    ssh np1
    cd /root/ployz
    UC='./bin/uc --connect unix:///run/uncloud/uncloud.sock'
    $UC deploy -f compose.prod.yaml -y ployz-web        # one service
    $UC deploy -f compose.staging.yaml -y               # all of staging

A changed env file only takes effect after its service is redeployed. Deploy redis-inngest before inngest, and inngest
before web and worker.

See [Migrations](#migrations) before changing the web or worker image.

## Migrations

Database migrations run through Uncloud's pre-deploy hook on web (`x-pre_deploy` on `ployz-web` and
`ployz-staging-web`):

    x-pre_deploy:
      command: ["npm", "run", "db:migrate"]
      timeout: 10m

- When a deploy replaces web's container, Uncloud first runs this command in a one-off container that uses the same
  image, the image's entrypoint, and web's env, `DATABASE_URL` included, from its env file. `db:migrate` applies the
  main and the analytics migrations. drizzle-kit skips migrations that are already applied, so a deploy that brings
  none only checks.
- If the hook fails or exceeds its timeout, the deploy stops before any web container is replaced. `uc` prints the
  hook's last 10 log lines and keeps the failed hook container (`ployz-web-pre-deploy-*`), which carries web's env:
  read its logs, never its config, then remove it.
- Uncloud skips the hook when the deploy changes nothing. Only web has the hook: worker shares its image, so the
  migrations run once.
- Deploy web before worker when an image brings migrations, so the worker never runs against an unmigrated schema.

Before any deploy that brings new migrations, take a backup. The hook doesn't do this for you. List the migrations
the bump adds with `git diff --name-only <old> <new> -- dashboard/drizzle`, then run:

    pscale backup create ployz main --org ployz --name pre-<version>-$(date -u +%Y%m%d%H%M)

Wait until `pscale backup list ployz main --org ployz` shows the backup's state as `success`.

Rolling back:
- Migrations are forward-only. To go back to an older image, redeploy the previous compose (keep a copy, for example
  `compose.prod.yaml.<version>`). Its hook finds nothing new to apply.
- That's safe only when the newer migrations are backward-compatible with the older code. Check every new
  migration before the bump and write down any SQL a rollback needs. beta.7's widened
  `operation_approvals_one_pending_idx` was one case: going back to 487748a needed the old index recreated.
- As a last resort, restore the backup into a new branch with `pscale backup restore`. That loses every write made
  since the backup.

## Networking

- Inngest is internal only: it has no published port and no Caddy route. web and worker reach it at
  `ployz-inngest.internal`.
- site and tools-api are internal: web reaches site through `MARKETING_ORIGIN`, and site's own Caddy proxies
  `/tools/api/*` to `TOOLS_API_UPSTREAM`.
- ployz.dev is proxied by Cloudflare (SSL mode Full (strict)). web trusts `X-Real-IP` because `RAILWAY_ENVIRONMENT_ID`
  is set to a placeholder. Caddy sets `X-Real-IP` from `CF-Connecting-IP` only when the peer is a Cloudflare edge,
  and from the peer address otherwise.
- `uptime-control.ployz.dev` (DNS-only) is where remote uptime agents pull checks and post results. Every call carries
  `UPTIME_AGENT_TOKEN`. Agents built from 487748a accept only `http://` control URLs, so remote agents need a local
  HTTPS forwarder.
- `dns.ployz.app` and `staging.ployz.dev` are DNS-only, so Caddy sees the client directly.

## Site rebuild

`ployz-site-rebuild` replaces Railway's hourly `site-rebuild` cron. It runs `site-rebuild/rebuild.sh` at minute 0
(supercronic). Each run does the following:

- Shallow-clones the head of getployz/ployz-site `main` over SSH and rebuilds the site with `--no-cache`. New site
  commits, the latest docs from getployz/ployz, and articles whose date has come all go live within the hour.
- Redeploys `ployz-site` only when the built pages changed. It writes `SITE_TAG` (`<commit>-<timestamp>`) to
  `/root/ployz/.env`, which `compose.prod.yaml` reads.
- Keeps the three newest rebuilt images.

The clone authenticates with a read-only deploy key on getployz/ployz-site, kept on np1 in
`/root/ployz/site-rebuild-key/`:

- `id_ed25519` is the private key, mode 0600 in a 0700 directory. It is mounted read-only and never committed.
- `known_hosts` holds GitHub's published SSH host keys from `https://api.github.com/meta`.

`GIT_SSH_COMMAND` in the compose file sets `IdentitiesOnly=yes` and `StrictHostKeyChecking=yes`. If GitHub rotates
its host keys, refresh the file:

    curl -s https://api.github.com/meta | jq -r '.ssh_keys[] | "github.com " + .' | ssh np1 'cat > /root/ployz/site-rebuild-key/known_hosts'

To run a rebuild now:

    ssh np1 'docker exec $(docker ps -q --filter label=uncloud.service.name=ployz-site-rebuild) /usr/local/bin/rebuild.sh'

## Logs

Uncloud's default `local` log driver drops a container's logs when the container is replaced, and that happens on
every deploy. np1's alloy collects every container's output and writes it to VictoriaLogs, keeping it for 7 days, with
the labels `service` (the Uncloud service name), `container`, `container_id` and `stream`. Those logs outlive the
container. To read them on np1:

    ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' $(docker ps -q --filter label=uncloud.service.name=victorialogs))
    vl() { docker run --rm --network uncloud curlimages/curl:8.11.1 -sS "http://$ip:9428/select/logsql/query" --data-urlencode "query=$1"; }
    vl '_time:1h service:="ployz-web"'                          # one service
    vl '_time:24h container:="ployz-web-oaov"'                  # one container, including replaced ones
    vl '_time:24h service:="ployz-web" | stats by (container) count()'

`:=` matches exactly: `service:ployz-site` would also match `ployz-site-rebuild`. The service names are ployz-web,
ployz-worker, ployz-inngest, ployz-redis-inngest, ployz-site, ployz-site-rebuild, ployz-tools-api, ployz-uptime,
ployz-uptime-agent-eu, ployz-hosted-dns and ployz-relay (staging: ployz-staging-*). For a running container,
`docker logs $(docker ps -q --filter label=uncloud.service.name=<service>)` also works.
