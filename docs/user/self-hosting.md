---
title: Self-host Ployz Cloud
description: Run the Ployz dashboard on your own infrastructure with Docker Compose.
---

You can run Ployz Cloud, the dashboard at ployz.dev, on your own server instead of using ours. It
runs from the released Docker image with your own GitHub apps. Billing is off, so every
organization gets custom domains.

You need:

- A Linux server with Docker Compose that can run `linux/amd64` images.
- A public https address for the dashboard, like `https://cloud.example.com`.
- A TLS proxy, like Caddy or nginx, that forwards that address to port 3000.

Everything Ployz Cloud stores, your organizations, projects and settings included, is in one
Docker volume, `ployz-cloud-self-host_postgres-data`. Back it up.

Your Ployz Cloud still uses a few parts Ployz hosts: the relay at `relay.ployz.dev` that the
dashboard reaches your servers through, the hosted DNS that gives each organization its domain
for generated addresses, the installer at `ployz.sh`, and the release binaries on GitHub.

## 1. Create the GitHub apps

Ployz Cloud needs two GitHub apps: an OAuth App for signing in, and a GitHub App for reading
repositories. In both, replace `https://cloud.example.com` with your address.

### OAuth App

Create it in GitHub under **Settings → Developer settings → OAuth Apps**:

| Field | Value |
| --- | --- |
| Homepage URL | `https://cloud.example.com` |
| Authorization callback URL | `https://cloud.example.com/api/auth/callback/github` |

Copy its **Client ID** to `GITHUB_CLIENT_ID`, and a new client secret to `GITHUB_CLIENT_SECRET`.

### GitHub App

Create it under **Settings → Developer settings → GitHub Apps**, on your account or your GitHub
organization:

| Field | Value |
| --- | --- |
| Homepage URL | `https://cloud.example.com` |
| Webhook URL | `https://cloud.example.com/api/github/webhook`, with **Active** checked |
| Webhook secret | The output of `openssl rand -hex 32`. Also put it in `GITHUB_APP_WEBHOOK_SECRET`. |
| Repository permissions | Contents: Read-only. Checks: Read and write. Metadata: Read-only. Actions: Read and write. Pull requests: Read-only. |
| Subscribe to events | Push, Check suite, Workflow run, Pull request |

**Actions** and **Workflow run** let Ployz build on GitHub Actions. **Pull requests**,
**Checks** and **Pull request** drive preview environments and the "Ployz · ready to merge"
check. GitHub sends installation events without a subscription.

Then fill in:

- `GITHUB_APP_ID`: the app's **App ID**.
- `GITHUB_APP_SLUG`: the app's name in its URL, `github.com/apps/<slug>`.
- `GITHUB_APP_PRIVATE_KEY`: a private key you generate on the app's page. Paste the whole PEM,
  in quotes.

If you later add a permission to the app, existing installations keep working without it until
their owner approves the new permissions on GitHub.

## 2. Configure and start

> [!WARNING]
> Keep `APP_ENCRYPTION_SECRET` the same forever. It encrypts the credentials Ployz Cloud stores,
> so a new value makes them unreadable. Back it up with the Postgres volume.

1. Download `ployz-cloud-compose.yml` and `ployz-cloud.env.example` from the latest
   [release](https://github.com/getployz/ployz/releases) into one directory:

   ```sh
   curl -fsSLo compose.yml https://github.com/getployz/ployz/releases/latest/download/ployz-cloud-compose.yml
   curl -fsSLo .env https://github.com/getployz/ployz/releases/latest/download/ployz-cloud.env.example
   ```

2. Fill in `.env`. Every variable that isn't commented out is required: `web` and `worker` stop
   with a `ConfigError` if one is missing. Generate each secret with `openssl rand -hex 32`. Leave
   every `POLAR_*` variable unset; that's what turns billing off.
3. Create the database tables, then start everything:

   ```sh
   docker compose run --rm web npm run db:migrate
   docker compose up -d
   docker compose ps
   ```

4. Point your TLS proxy at port 3000, or at `WEB_PORT` if you set it.

`worker` turns healthy once it connects to Inngest. Nothing migrates the database on start, so
run the migration on every install and upgrade.

## 3. Sign in, then install the GitHub App

1. Open your address and sign in with GitHub.
2. Install the GitHub App as the same GitHub user, from the dashboard or from
   `github.com/apps/<slug>`.

Sign in first: Ployz Cloud links an installation only to a GitHub account that has already signed
in.

The command the dashboard gives you to [add a server](servers/add-a-server.md) already points at
your address. The CLI signs in to ployz.dev unless you point it at yours with
`ployz login --cloud-url https://cloud.example.com`. In CI, set `PLOYZ_CLOUD_URL` next to
`PLOYZ_TOKEN`; see [Make an organization token](cli/overview.md#make-an-organization-token).

## Upgrade

Download the new release's `ployz-cloud-compose.yml` over `compose.yml`, then:

```sh
docker compose pull
docker compose run --rm web npm run db:migrate
docker compose up -d
```

`docker compose up -d` can take up to 30 minutes: `worker` finishes the work it's running before
it stops.
