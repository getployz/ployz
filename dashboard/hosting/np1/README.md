# Ployz hosted stack on np1

Ployz Cloud (ployz.dev), its staging copy (staging.ployz.dev), and the hosted-dns API (dns.ployz.app) run on np1, a
Hetzner root server at 5.9.85.203 / 2a01:4f8:162:4cb::2. np1 is a single-machine Uncloud cluster (uncloudd 0.19), and
Uncloud's Caddy terminates TLS. np1 also runs other projects; these files touch only the `ployz-*` services, the
`ployz-*` volumes and the routes they publish.

| File | Purpose |
|---|---|
| `compose.prod.yaml` | web, worker, inngest, redis-inngest, site, tools-api, uptime, uptime-agent-eu, hosted-dns |
| `compose.staging.yaml` | staging web, worker, inngest, redis-inngest, postgres |
| `hosted-dns.Dockerfile` | builds getployz/hosted-dns (Railway used Railpack) |

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

Database migrations do not run at boot. Before deploying a web/worker image that adds migrations, run them once from
that image:

    docker run --rm --env-file <file with DATABASE_URL only> --entrypoint npm <image> run db:migrate

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

## Logs

    ssh np1 docker logs --since 1h $(docker ps -q --filter name=ployz-web-)

Uncloud's default `local` log driver drops a container's logs when the container is replaced, and that happens on
every deploy.
