---
title: Production checklist
description: What to set up before you move real users to your app, and what Ployz doesn't do for you yet.
---

Ployz is young and changing quickly. Before you move real users to your app, work through this
list. Each item links to the page that does it.

> [!WARNING]
> Ployz doesn't back up databases or volumes yet. A volume lives on one server and isn't copied
> anywhere: if that server's disk is lost, so is the data.

## Before you move real users

- **Back up your data.** Dump your databases on a schedule and keep the dumps off your servers;
  this is CLI-only for now. See [Back up a database](services/databases.md#back-up-a-database).
  Or keep your hosted database and [point your app at it](services/databases.md#use-a-managed-database-instead).
- **Add a health check.** Without one, a new replica gets traffic a few seconds after it starts,
  ready or not. See [Deploy without downtime](deploy/deployments.md#deploy-without-downtime).
- **Run two replicas on two servers.** With one replica on one server, one failure takes your
  app down. [Add a second server](servers/add-a-server.md), then
  [run more replicas](services/scaling.md#run-more-replicas). A service with a volume runs one
  replica, on the volume's server.
- **Set CPU and memory limits.** Otherwise one busy service can use all of its server's CPU and
  memory and starve the others. See [Scaling](services/scaling.md#run-more-replicas).
- **Wait for CI.** A push deploys at once, even when your tests fail. See
  [Wait for CI](deploy/github.md#wait-for-ci).
- **Use your own domain.** Generated addresses are for getting started. Custom domains need Pro,
  at $9 a month. See [Add a custom domain](services/domains.md#add-a-custom-domain).
- **Look after your servers.** Ployz doesn't install OS updates or harden your servers. Log in
  with SSH keys, not passwords, and turn on automatic security updates (`unattended-upgrades` on
  Ubuntu). If you add a firewall, keep [these ports](servers/add-a-server.md#open-the-firewall)
  open. A reboot stops the services on that server until it's back.
- **Watch your app yourself.** Ployz doesn't send alerts when a service crashes or a server goes
  offline. Point an uptime monitor at your app's address, ideally at its
  [health check](deploy/deployments.md#deploy-without-downtime) path.

## What Ployz doesn't do yet

- **No automatic failover.** If a server dies, the services that ran only there stop, and a
  database on it is down until the server is back. See
  [When a server goes down](services/scaling.md#when-a-server-goes-down).
- **No rollbacks.** To go back, revert the commit and push, or change the setting back and
  deploy.
- **No Docker Compose files.** Add each container as its own service.
- **No autoscaling.** You choose the number of replicas.
- **No teams.** Ployz is built for one person today.

Backups, snapshots, rollbacks and moving a database to a bigger server without downtime are
planned.
