---
title: Deploy without downtime
description: Keep your app serving through a deploy, choose how replicas are replaced, and what happens when a server is cut off.
---

With **Start first**, the default for a service without a volume, Ployz starts the new replicas
alongside the old ones and waits until they're all healthy. Then every server starts sending
the service's requests to the new replicas. Once every server has, the old replicas get no new
requests, finish the ones they have, and stop. An old replica with no open connection stops at
once; one still answering a request gets up to 3 seconds. A replica that listens on no port,
such as a worker, gets the 3 seconds whenever it holds a connection to anything outside itself.
Your app keeps serving the whole time.

## Keep your app serving through a deploy

- **Set a health check.** Under **Settings → Deploy**, set **Healthcheck** to an HTTP path like
  `/up` or a command like `pg_isready -h 127.0.0.1`. A new replica is ready when the path answers
  with a 2xx status or the command exits 0. A redirect, to `https://` or `/login` for example,
  fails the check. Without a health check, the old replica stops about a second after the new one
  starts, ready or not. A new replica that crashes after that second doesn't fail the deploy, and
  `ployz status` names it. The databases Ployz creates come with one.
- **A service with a [volume](../services/volumes.md) has a short gap.** Its old replica stops
  first, so two never write the same files. To change that, see
  [Choose how a deploy replaces replicas](#choose-how-a-deploy-replaces-replicas).
- **Migrations run first.** A **Pre-deploy command**, like `npm run migrate`, runs once with the
  new code before any replica is replaced. If it fails or runs past 5 minutes, the deployment
  fails and the old version keeps running.
- **Services that depend on each other start in order.** If `web`'s
  [variables](../services/variables.md) reference `${{ postgres.DATABASE_URL }}`, `postgres`
  switches first, and passes its health check first if it has one. Services that don't depend on
  each other deploy at the same time.

## Choose how a deploy replaces replicas

Under **Settings → Deploy**, **Update order** picks how a deploy swaps old replicas for new ones.
With **Start first** or **Stop first**, visitors reach the new version only once every new replica
is ready. With **Rolling**, they reach each server's new replica as soon as it's ready.

| Update order | What happens | Downtime |
| --- | --- | --- |
| **Start first** | New replicas start beside the old ones, and the old ones stop once the new ones are ready. Two copies run for a moment. | None |
| **Stop first** | On every server at once, each old replica stops, then its replacement starts. Traffic moves to the new replicas when they're all ready. | From the stop until every new replica is ready |
| **Rolling** | Stop first, one server at a time. A server's new replica takes traffic once it's ready, before the next server's old replica stops, so old and new versions serve together. If a server's new replica fails, the servers after it keep the old version. | None with replicas on two or more servers. On one server, as Stop first |
| **Automatic** | **Stop first** for a service with a volume, **Start first** for the rest. | As above |

**Automatic** is the default. A service with a volume can use **Start first** only when the volume
allows shared writes, because both copies write to it at once. Otherwise Ployz refuses the change
and says to choose **Stop first** or allow shared writes.

If a new replica fails, Ployz starts the old replicas it stopped again, whichever order you chose.

## When a server is cut off during a deploy

Ployz waits up to five minutes for each server to switch. If a server is cut off or powered off,
the deploy keeps the new replicas, leaves the old one running there, exits 3, and says so. When
web-2 was already down as the deploy started:

```text
! If web-2 restarts and confirms the new copies within a few minutes of starting, it takes its replaced copies out of traffic. Otherwise they keep answering beside the new copies. Either way they keep running, and a left worker still takes jobs, until the next deploy.
```

When web-2 stopped answering during the deploy:

```text
! my-app-production/web left running on web-2 (web-2 did not answer). The next deploy cleans it up.
```

Removing a service while a server that may run one of its replicas is down, or Ployz couldn't
check it, removes the replicas on the servers that answer, without waiting for the proxies to
drop them. The deploy names that server and exits 3:

```text
rmme: removed 2/3; web-2 didn't answer, the next full deploy removes its copy once it is back
```

The replica on web-2 may serve again once that server is back. Your next deploy of the whole
environment, with pruning on, removes it and names it.

If web-2 stops answering in the middle of the switch, the deploy can fail instead, with
`failed (2/3 ready)`, and exit 1, even though the new replicas serve
([#2600](https://github.com/getployz/ployz/issues/2600)). Deploy again once web-2 is back.

If the connection to a server drops while a new replica starts, the deploy fails with `create
Container failed` and a cause such as `connection lost`. The server may have started that replica
anyway. It keeps running beside the old ones but takes no requests, and the old replicas keep
serving. `ployz ps` lists both, and the deployment doesn't mention the extra one. Your next deploy
removes it, whatever you changed.

Until you deploy again, that server can keep answering with the old version. See
[Old and new versions answer at once](../troubleshooting/deployments.md#old-and-new-versions-answer-at-once).
