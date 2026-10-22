---
title: Production checklist
description: What to set up before you move real users to your app, and what Ployz doesn't do for you yet.
---

Ployz is young and changing quickly. Before you move real users to your app, work through this
list. Each item links to the page that does it.

> [!WARNING]
> A volume lives on one server. Without a [mirror](services/mirror-a-volume.md), if
> that server's disk is lost, so is the data.

## Before you move real users

- **Back up your data.** Add a [mirror](services/mirror-a-volume.md) to each volume
  you can't lose, so it has a backup on another of your servers that you can
  [restore](services/restore-a-volume.md). A mirror isn't offsite, so also dump your databases on
  a schedule and keep the dumps off your servers. See
  [Back up a database](services/databases.md#back-up-a-database).
  Or keep your hosted database and [point your app at it](services/databases.md#use-a-managed-database-instead).
- **Add a health check.** Without one, a new replica gets traffic about a second after it starts,
  ready or not. A crash after that second doesn't fail the deploy, and `ployz status` names the
  replica. See [Deploy without downtime](deploy/deployments.md#deploy-without-downtime).
- **Run two replicas on two servers.** With one replica on one server, one failure takes your
  app down. [Add a second server](servers/add-a-server.md), then
  [run more replicas](services/scaling.md#run-more-replicas). A service with a volume runs on the
  volume's server, one replica unless the volume [allows shared writes](services/volumes.md#share-a-volume).
- **Set CPU and memory limits.** Otherwise one busy service can use all of its server's CPU and
  memory and starve the others. See [Scaling](services/scaling.md#run-more-replicas).
- **Keep production Protected.** A new project's production starts Protected, so only an owner
  or admin can change where it deploys from, and nobody can delete it until it's unprotected.
  Check it under **Settings → Environment**. See [Protect an environment](environments/environments.md#protect-an-environment).
- **Wait for CI.** A push deploys at once, even when your tests fail. See
  [Wait for CI](deploy/github.md#wait-for-ci).
- **Use your own domain.** Generated addresses are for getting started. See
  [Add a custom domain](services/domains.md#add-a-custom-domain).
- **Look after your servers.** Ployz doesn't install OS updates or harden your servers. Log in
  with SSH keys, not passwords, and turn on automatic security updates (`unattended-upgrades` on
  Ubuntu). If you add a firewall, keep [these ports](servers/add-a-server.md#open-the-firewall)
  open. A reboot stops the services on that server until it's back.
- **Watch your app.** Ployz Cloud checks every service with a public address and shows its
  [uptime](observe/uptime.md) next to its [metrics](observe/metrics.md) and
  [logs](observe/logs.md). It emails each member when a service goes down, comes back up, or
  keeps crashing. Each person turns these on or off for themselves in
  [Notifications](account/notifications.md).

## What Ployz doesn't do yet

- **No automatic failover.** If a server dies, the services that ran only there stop, and a
  database on it is down until the server is back. See
  [When a server goes down](services/scaling.md#when-a-server-goes-down).
- **No automatic rollbacks.** A failed deploy doesn't undo itself. To go back, click **Redeploy**
  on an earlier deployment; it ships that deployment's commit and settings. See
  [Redeploy or cancel a deployment](deploy/deployments.md#redeploy-or-cancel-a-deployment). The
  rollback lasts until your next push or deploy, so to keep it, revert the commit in Git. To roll
  back settings only, use **Roll back to #N** in **Deployments** > **Versions**, which stages
  them, then click **Deploy**; see
  [Saved history](deploy/deployments.md#saved-history).
- **No Docker Compose files.** Add each container as its own service.
- **No autoscaling.** You choose the number of replicas.
- **Three roles.** An organization has owners, admins and members, with no finer permissions. See
  [Invite people](account/organizations.md#invite-people).

