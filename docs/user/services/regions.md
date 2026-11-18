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
region, the deploy moves the volume to a server in the region first, then starts the service there.
`ployz deploy --plan` and Details in the dashboard list the move:

```
postgres → fsn
pg-data moves db-1 → web-1
```

The volume goes to a server in the region that accepts services. The one that holds its
[mirror](mirror-a-volume.md) comes first. With no mirror there, it goes back to one that holds its
[old copy](move-a-volume.md#the-old-copy-a-move-leaves) and sends only what changed since. With
neither, it goes to the first by name with managed volumes and room for it. To choose the server
yourself, stage the move with **Make primary** before you deploy. See
[Move a volume](move-a-volume.md) for what a move does to the service while it runs.

If the move fails, the deploy doesn't start that service and the rest of the deploy goes ahead.
The deploy says why the volume didn't move:

```
! pg-data didn't move to web-1: web-1 stopped answering
postgres: not started (pg-data is on db-1)

next: ployz deploy --project shop --env production
```

A move that fails before it stops the service leaves the volume where it was, with the service
running there. One that fails later can leave the service stopped.
[A move failed partway](../troubleshooting/volumes.md#a-move-failed-partway) says what each stage
leaves and how to finish it.

Deploy again to try the move again. `ployz deploy --plan` repeats why the last deploy didn't move
the volume:

```
pg-data moves db-1 → web-1
The last deploy didn't move pg-data: web-1 stopped answering
```

Don't move the volume by hand with `ployz volume move`. The service still has the placement of its
last deploy, which doesn't run it on `web-1`, so Ployz
[refuses that move](move-a-volume.md#when-the-service-cant-start-on-the-new-server) before it
stops anything. The deploy moves the volume and starts the service with its new placement.

If you change the region again while a deploy is still moving the volume, the deploy you start
then doesn't move it a second time. Deploy again once the earlier deploy ends.

The deploy also leaves the volume where it is when:

- **You have no Ployz Cloud.** Moving a volume needs it, so the deploy prints no command. Give the
  volume's server the region, or change the environment's region.
- **A mirror of the volume is on a server outside the region and none is inside it.** A volume
  moves only onto a server that holds its mirror while it has one. The plan lists no move, and
  the deploy prints the command that removes the mirror:

  ```
  postgres: not started (pg-data is on db-1)

  next: ployz volume mirror rm pg-data-db-2 --project shop --env production
  ```

  Run it, then deploy again.
- **Your servers enforce device approval and no approved device or passkey approved the move.**
  That is a deploy from a push, the agent, or `ployz` on a computer without an approved device
  key. `ployz deploy --plan` there says so:

  ```
  pg-data stays on db-1: your servers move it only for a deploy an approved device signed
  ```

  A deploy from the dashboard asks for your passkey and moves the volume. `ployz deploy` on
  an approved device shows the move after the deploy's own lines and asks before it signs it:

  ```
  Approved: move pg-data from db-1 to web-1. postgres stops on db-1 for the last copy, then starts on web-1.
  ```

  Below that line is `postgres` as it starts on `web-1`, as
  [a move's approval](move-a-volume.md#servers-that-enforce-device-approval) lists it. Its
  placement there is the last deploy's, so it can name the old region. The deploy's own lines
  above it name the new one. Answer no and nothing is deployed. `--yes` signs without asking, as
  it does for the deploy.
- **Your servers enforce device approval, the deploy came from `ployz`, and the service has a
  [count for each region](#run-a-service-in-more-than-one-region).** `ployz deploy` signs a move
  only into the environment's region, and says so:

  ```
  ! pg-data stays on db-1. This device signs a move only into the environment's region, and postgres has a count for each region.
  ```

  A deploy from the dashboard asks for your passkey and moves the volume. From `ployz`, move the
  volume first and leave the service stopped, then deploy to start it:

  ```sh
  ployz volume move pg-data --to web-1 --leave-stopped
  ployz deploy
  ```

A move deletes an [old copy](move-a-volume.md#the-old-copy-a-move-leaves) of the volume on any
other server first. That happens when a third server holds one, for example after `fsn` to `syd`
to `lhr`. The deploy moves the volume only when you accept that delete:

- **From the dashboard**, Details lists the delete and the deploy asks you to confirm it. On
  servers that enforce device approval, it asks for your passkey for both.
- **From `ployz deploy`, a push or the agent**, the deploy leaves the volume where it is and
  doesn't start that service. The rest of the deploy goes ahead. `ployz deploy --plan` prints the
  command that deletes the copy, and the deploy ends with it as its `next:` line:

  ```
  pg-data stays on db-1. Delete its old copy on web-2 first: ployz volume mirror rm pg-data-web-2 --if-old-copy
  ```

  Run it, then deploy again:

  ```sh
  ployz volume mirror rm pg-data-web-2 --if-old-copy
  ployz deploy
  ```

  `ployz deploy --accept-volume-loss pg-data` accepts the delete instead, and the deploy moves the
  volume.

A Docker volume can't move. Give its server the region, or change the environment's region.
`ployz deploy --plan` says the service won't start, and the deploy doesn't start it:

```
postgres → fsn: won't start (pg-data is a Docker volume and can't move)
```

```
postgres: not started (pg-data is a Docker volume and can't move)
```

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
- **An older `ployz` still deploys an environment with regions.** Its deploys follow the
  environment's region and each service's counts. It shows a count for each region as
  `{"fsn":2,"syd":1}`, and can set one. It doesn't show the environment's region: `ployz get`
  leaves it out, and `ployz diff` leaves out a staged change to it, which `ployz deploy` still
  applies. [Upgrade the CLI](../cli/overview.md#install-the-cli) to see or set the region.
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
