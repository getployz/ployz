---
title: Regions
description: Keep an environment's services on the servers in one region, such as the ones nearest your users.
---

A region is a name you give your servers, such as `fsn` for Falkenstein or `syd` for Sydney. Give
an environment a region, and every service in it runs on the servers that carry that name. Without
one, its services run on any server.

Regions are CLI-only for now.

## Give a server a region

A server is in a region when its `region` label names it:

```sh
ployz server set web-1 --label-add region=fsn
```

Choose any short name. Servers with the same name are in the same region. Upper and lower case
are the same, so `FSN` is `fsn`.

## Set an environment's region

1. Check what a deploy would do. Each service shows the region it would run in:

   ```sh
   ployz env region fsn
   ployz deploy --plan
   ```

   ```
   web → fsn
   postgres → fsn
   ```

2. Deploy. Every service restarts, and its new replicas start on the region's servers.

   ```sh
   ployz deploy
   ```

The region is a staged change like any other: `ployz diff` shows it as `@environment.region`,
and `ployz discard @environment.region` drops it. The [Ployz agent](../account/agent-sidebar.md)
can stage a region too. Its deploy waits for your approval, which shows the region.

Ployz refuses a region none of your servers is in, and stages nothing:

```
error: No Server is in lhr. Regions: fsn, syd.
```

Ployz checks this when it can read your servers' labels. If no server answers, or the agent runs
without Ployz Cloud, the region is staged unchecked, and the deploy reports the services it
couldn't start. See [When no server is in the region](#when-no-server-is-in-the-region).

To run on any server again:

```sh
ployz env region --clear
ployz deploy
```

## When a volume is outside the region

A service with a [volume](volumes.md) runs on the volume's server. If that server is outside the
region, the deploy doesn't start the service, and names the volume and where it is:

```
postgres: not started (pg-data is on db-1)
```

The rest of the deploy goes ahead, and the service keeps running where it was. Move the volume
with the command the deploy prints, then deploy again:

```sh
ployz volume move pg-data --to web-1
ployz deploy
```

See [Move a volume](move-a-volume.md) for what a move does to the service while it runs.

Moving a volume needs Ployz Cloud. Without it, give the volume's server the region, or change the
environment's region.

## When no server is in the region

If the region's last server loses its label or is removed, the next deploy doesn't start the
environment's services and reports each one:

```
web: not started (no Server in fsn)
```

If the region has servers but none can run services, the line names them:

```
web: not started (fsn-1 is down, and no other Server in fsn is up)
```

Replicas that already run keep running. Give a server the region, bring its server back, or change
the environment's region, and deploy again.

## Good to know

- **One server in a region holds every replica.** Ployz spreads replicas over the region's
  servers only. With one server there, all of a service's replicas run on it, and they all go
  down with it. Add a second server to the region before you rely on replicas.
- **A branch starts with its parent's region** and follows later changes to it, like the parent's
  other settings.
- **Syncing two environments doesn't copy the region.** Set it on each.
- **Setting or clearing a region restarts every service**, including one whose replicas already
  run on the region's servers. A service with a volume is down while it restarts.
- **Only deploys move replicas.** Changing a server's label or an environment's region changes
  nothing until you deploy.
