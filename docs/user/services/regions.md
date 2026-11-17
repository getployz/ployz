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

Choose any short name made of letters, digits, hyphens, underscores and dots. A label with other
characters, such as a space, is not a region. Servers with the same name are in the same region.
Upper and lower case are the same, so `FSN` is `fsn`.

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

2. Deploy. A replica already on one of the region's servers keeps running. A replica outside the
   region is replaced by a new one on the region's servers.
   [Good to know](#good-to-know) lists when a replica in the region restarts anyway.

   ```sh
   ployz deploy
   ```

The region is a staged change like any other: `ployz diff` shows it as `@environment.region`,
and `ployz discard @environment.region` drops it. In Ployz Cloud, the environment's bottom bar
counts it and **Details** lists it as **Region**, where you can deploy or discard it. The
[Ployz agent](../account/agent-sidebar.md) can stage a region too. Its deploy waits for your
approval, which shows the region.

Ployz refuses a region none of your servers is in, and stages nothing:

```
error: No Server is in lhr. Regions: fsn, syd.
```

If no server answers, `ployz env region` stages the region unchecked, and the deploy reports the
services it couldn't start. See [When no server is in the region](#when-no-server-is-in-the-region).
The agent asks you to try again once your servers answer. An agent without Ployz Cloud stages the
region unchecked.

To run in any region again:

```sh
ployz env region --clear
ployz deploy
```

## Run a service in more than one region

A service usually runs all its replicas in the environment's region. To spread it, give it a count
for each region:

```sh
ployz set api.replicas='{"fsn":2,"syd":1}'
ployz deploy
```

`api` now runs two replicas on the servers in `fsn` and one on a server in `syd`, under one name in
`ployz ps`. The counts take the place of the environment's region for that service. The
environment's other services stay in its region.

Change a count and deploy again. Ployz adds or removes only the difference: going from
`{"fsn":2,"syd":1}` to `{"fsn":1,"syd":1}` removes one `fsn` replica and leaves the other two
running.

Ployz refuses a count for a region none of your servers is in, and stages nothing:

```
error: No Server is in lhr. Regions: fsn, syd.
```

To put the service back in the environment's region, set one number:

```sh
ployz set api.replicas=3
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

A service with a count for each region isn't started when any of its regions has no server that
can run it. The deploy names every such region, and the service's replicas in its other regions
keep running as they were:

```
api: not started (no Server in syd)
```

## Good to know

- **One server in a region holds every replica.** Ployz spreads replicas over the region's
  servers only. With one server there, all of a service's replicas run on it, and they all go
  down with it. Add a second server to the region before you rely on replicas.
- **A service with a volume runs in one region.** Its replicas all write to the volume, which is
  on one server. Ployz refuses counts in two regions for it.
- **A preview environment runs its one replica in the region with the most replicas**: `syd` for
  `{"fsn":1,"syd":3}`. When regions tie, it is the first by name.
- **A drain keeps a replica in its region when the service has a count for that region.** The
  replica moves to another server there, or stays when there is none. If its server's region is
  no longer one the service has a count for, it moves to a server in any region that is. A
  service with one number moves within the environment's region, or to any server when the
  environment has none.
- **Upgrade every `ployz` before you set a count for each region.** An older `ployz` doesn't
  know them. [Upgrade the CLI](../cli/overview.md#install-the-cli) on each computer that deploys.

  With Ployz Cloud, an older `ployz` still shows the environment, and its deploys follow your
  counts. On servers that
  [enforce device approval](../deploy/deployments.md#deploy-to-servers-that-enforce-device-approval),
  its deploy fails while a service has a count for each region:

  ```
  error: web: create Container failed.
    cause: This service's settings differ from what the device approved.
  ```

  Without Ployz Cloud, an older `ployz` can't read the environment once a count for each region
  is staged:

  ```
  error: The Config Store holds an unreadable Working State
  ```

  After you deploy a count for each region, an older `ployz` can't deploy that environment
  again. An older `ployz ps` works again once every service has one number, the environment has
  no region, and you deploy with the new `ployz`. An older `ployz project rm` still fails, and
  leaves the removal queued. Run `ployz project rm` with the new `ployz` to finish it.
- **A branch starts with its parent's region** and follows later changes to it, like the parent's
  other settings.
- **Syncing two environments doesn't copy the region.** Set it on each.
- **A replica that can stay keeps running** when the deploy changes only the region and the
  number of replicas. A replica outside the new region is replaced by one inside it. After you
  clear a region, or change it to one with more servers, replicas spread again: if several share
  a server, one moves to another server. The new replica starts before the old one stops, so the
  service keeps answering.
- **A service restarts every replica** when the same deploy changes anything else about it, such
  as its image, a limit or its pre-deploy command. It also restarts every replica when one of
  them isn't running, when any server running one of them runs an older Ployz, or when it is a
  global service. A service with a volume is down while it restarts.
- **Only deploys move replicas.** Changing a server's label or an environment's region changes
  nothing until you deploy.
