# Ployz docs

Ployz turns your Linux servers into a deployment platform: Git push deploys, preview
environments, databases and https, on servers you own. These docs cover the dashboard
at [ployz.dev](https://ployz.dev).

Writing or editing a page? Read [STYLE.md](STYLE.md) first.

## Start here

New to Ployz? Follow these steps from a first deploy to real users.

1. **Deploy an example app:** [Quick start](getting-started/quick-start.md)
2. **Deploy your own app:** [Deploy from GitHub](deploy/github.md)
3. **Configure it:** [Variables](services/variables.md) and [Databases](services/databases.md)
4. **Test changes:** [Preview environments](environments/preview-environments.md)
5. **Get ready for production:** [Production checklist](production-checklist.md)

## Getting started

- [Introduction](getting-started/introduction.md): whether Ployz fits, what you need, what it costs
- [Quick start](getting-started/quick-start.md): an example app on your server at an https address
- [The basics](getting-started/the-basics.md): how your app is organized and how a change ships

## Deploy

- [Deploy from GitHub](deploy/github.md): connect GitHub, deploy on push, wait for CI, monorepos
- [Deploy a Docker image](deploy/docker-image.md): public and private registries
- [Deployments](deploy/deployments.md): staged changes, Deploy, following and fixing a deployment

## Services

- [Services](services/services.md): add, rename, restart or delete a service
- [Service settings](services/settings.md): what each field does; the dashboard links here
- [Variables](services/variables.md): variables, secrets, references between services
- [Databases](services/databases.md): PostgreSQL, MySQL, Redis and MongoDB, connecting, backups
- [Volumes](services/volumes.md): files that survive deploys
- [Configs](services/configs.md): shared text files mounted read-only into services
- [Move a volume](services/move-a-volume.md): mirror a volume and move it to another server
- [Domains](services/domains.md): generated addresses, custom domains, https
- [Private networking](services/private-networking.md): how services reach each other
- [Scaling and multiple servers](services/scaling.md): replicas, more servers, when a server goes down

## Environments

- [Environments and branches](environments/environments.md): production, staging and branches of them
- [Preview environments](environments/preview-environments.md): a copy of your app for every pull request

## Builds

- [How builds work](builds/overview.md): what triggers a build, when one is reused, build logs
- [Railpack and Dockerfiles](builds/railpack-and-dockerfiles.md): building without a Dockerfile, build commands
- [Where builds run](builds/where-builds-run.md): your servers or GitHub Actions, and in what order
- [Build resources and cache](builds/build-resources-and-cache.md): limit what builds use, clear the cache

## Servers

- [Add a server](servers/add-a-server.md): requirements, the install command, the ports to open
- [Manage servers](servers/manage-servers.md): status, upgrades, removing a server

## Observe

- [Logs](observe/logs.md): service logs and deployment logs

## Account

- [Organizations and billing](account/organizations.md): your organization, deleting it, Pro

## More

- [Production checklist](production-checklist.md): before you move real users
- [CLI and coding agents](cli/overview.md): install the CLI, set up your coding agent, MCP tools, tokens for CI
- [Self-host Ployz Cloud](self-hosting.md): run the dashboard on your own infrastructure

## Troubleshooting

- [Builds](troubleshooting/builds.md): detection failures, out of memory, stuck builds
- [Deployments](troubleshooting/deployments.md): failed health checks, crashing services, queued deployments
- [My app isn't reachable](troubleshooting/app-not-reachable.md): 502s, wrong ports, domains and certificates
- [Servers](troubleshooting/servers.md): install failures, servers that don't join or go offline, full disks
