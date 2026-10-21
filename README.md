<p align="center">
  <img src="dashboard/public/assets/logo-mark.svg" width="64" height="64" alt="Ployz">
</p>

<h1 align="center">Ployz</h1>

<p align="center">
  <strong>Rent a server. Get a cloud.</strong>
  <br>
  Preview environments on servers you own. One command from a fresh server to https.
</p>

<p align="center">
  <a href="https://ployz.dev">Website</a> · <a href="#quick-start">Quick start</a> · <a href="#use-it-with-your-agent">Agents</a> · <a href="#status">Status</a> · <a href="#self-hosting-the-dashboard">Self-host</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/readme/ployz-up-dark.svg">
    <img src="docs/readme/ployz-up-light.svg" width="760" alt="A terminal running ployz up --server root@your-server. Ployz installs itself, builds the app, deploys it and prints its https address.">
  </picture>
  <br>
  <sub><code>ployz up --server root@…</code> on a fresh server, from the first command to the https address.</sub>
</p>

Ployz turns Linux servers you rent into a deployment platform. It builds your app, gives it an
https address and runs its databases and workers next to it. Every pull request can get its own
environment. Start with one server and add more when you need them.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/readme/canvas-dark.png">
    <img src="docs/readme/canvas-light.png" width="760" alt="The Ployz dashboard canvas with the service that ployz up created and its https address.">
  </picture>
  <br>
  <sub>The canvas that run produced. The CLI and the dashboard edit the same project.</sub>
</p>

<p align="center">
  The workflow of the hosted platforms you like, on servers you rent.
  <br><br>
  <img src="docs/readme/platforms.svg" width="560" alt="Logos of Vercel, Railway, Fly.io, Render, Netlify and Heroku">
  <br>
  <sub>Six hosted platforms. Ployz isn't affiliated with any of them.</sub>
</p>

## What's different

- **An environment for every pull request.** Each preview runs the pull request's code at its own
  https address. Copy the database service and the preview gets its own empty database, seeded by
  your setup command. A database you leave shared gives the preview production data. On merge,
  production gets the code and the preview's setting changes. The **Ployz · ready to merge** check
  lists those changes on the pull request first.
- **Review before you deploy.** Edits from the dashboard, the CLI or an agent wait as a draft. You
  read the diff, then deploy it.
- **The CLI does what the dashboard does.** Commands print JSON with `--json`. A change that would
  delete a service, a volume or a domain stops until a person approves it. An agent can't approve
  it.
- **One private network across your servers.** Add a server with `ployz server add`. Your servers
  share a WireGuard network, and a service runs up to 50 replicas across them. A service with a
  volume runs one replica, on the server that holds its data.
- **Apps that outlive Ployz Cloud.** Your apps keep running on your servers if Ployz Cloud is down
  or goes away. The engine and the dashboard are open source.

## Use it with your agent

Paste this into your coding agent.

```text
Set me up for Ployz: https://ployz.dev/agents.md
```

It installs the CLI and the Ployz skill, signs you in and registers the Ployz MCP tools. From
then on it runs the same commands you would.

```text
> Copy production with its own worker, deploy it there, then stage it back.
$ ployz env branch try-worker --copy worker   # Made Branch shop/try-worker of production.
$ ployz deploy --env try-worker               # 2 deployed · across 1 server
$ ployz env sync --env try-worker --to        # Synced try-worker → shop/production.
```

The change waits in production for you to review and deploy.

## Quick start

You need a Linux server with systemd that you can reach as root over SSH. See the
[server requirements](docs/user/servers/add-a-server.md#what-you-need).

1. Install the CLI.
   ```sh
   curl -fsSL https://ployz.sh | sh
   brew install getployz/ployz/ployz   # or with Homebrew
   ```

2. Sign in to Ployz Cloud.
   ```sh
   ployz login
   ```

3. From your app's directory, point Ployz at your server.
   ```sh
   ployz up --server root@your-server
   ```

   Ployz installs itself on the server, builds your app and prints its https address.

4. Deploy on every push, and give each pull request a preview with its own seeded database.
   ```sh
   ployz github connect                # install the Ployz GitHub App
   ployz set shop.repository=you/shop  # build from GitHub instead of uploads
   ployz deploy
   ployz env pr you/shop --on --from production --copy postgres --setup 'shop=pnpm db:seed'
   ```

## How it works

Each server runs a small Ployz daemon next to Docker. Your servers share a WireGuard network and
keep each other up to date. Every server serves https itself through Caddy, so there's no load
balancer to rent. The dashboard and the CLI tell your servers what to run. Once deployed, your
apps depend on neither. Generated `ployz.app` addresses use Ployz's DNS. A custom domain with an
A record pointing at your servers doesn't.

## Status

**Can I move a production app?** Yes, if the gaps below are fine for you. If you need managed
backups and automatic failover today, wait. Ployz is young and changes quickly. Work through the
[production checklist](docs/user/production-checklist.md) first.

- **You look after the OS.** Ployz doesn't install OS updates.
- **No offsite backups yet.** Volume mirrors keep a copy on another server. Dump your databases
  on a schedule and keep the dumps off your servers, or keep your hosted database.
- **No automatic failover.** If a server dies, what ran only there stops. Replicas on other
  servers keep running.
- **No Docker Compose files.** Add each container as its own service.
- **Alerts are email only.** Ployz Cloud emails you when a service goes down or crashes.

**Soon:** offsite backups, automatic rollbacks, and moving a database to a bigger server without
downtime.

## Pricing

Ployz is free on your own servers, custom domains included. Paid [Ployz Cloud](https://ployz.dev)
plans are coming soon. Your hosting provider bills you for the servers.

## Self-hosting the dashboard

Run the dashboard yourself instead of Ployz Cloud. See [dashboard/README.md](dashboard/README.md#self-host).

## How Ployz is built

Agents write most of Ployz. I design the loop they run in, and I answer for what comes out of it.

Every change ships as a small PR. The agent has to prove it by running Ployz the way you would,
on real servers. If it can't show me that, it doesn't merge. Low-risk PRs merge once the checks
pass. The orchestrator, networking and anything touching your data get a human review first.

When an agent gets something wrong, we turn the mistake into a lint rule or a test. Every mistake
makes the system stricter, and that's the part I trust.

## Contributing

Bug reports and feature requests go in [GitHub Issues](https://github.com/getployz/ployz/issues).

The repository holds two projects. [core/](core/README.md) is the engine, CLI and daemon, in Rust.
[dashboard/](dashboard/README.md) is Ployz Cloud, in TypeScript. Each README explains how to build it.

## License

The engine in `core/` is [Apache-2.0](core/LICENSE). The dashboard in `dashboard/` is [AGPL-3.0](dashboard/LICENSE).
