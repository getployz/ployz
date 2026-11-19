---
title: Regions
description: Keep an environment's services on the servers in one region, such as the ones nearest your users.
---

A region is a name you give your servers, such as `fsn` for Falkenstein or `syd` for Sydney. Give
an environment a region, and its services run on the servers that carry that name, unless a service
has [its own counts](#run-a-service-in-more-than-one-region). Without a region, its services run
on any server.

You set regions from the CLI and from the Servers page in Ployz Cloud.

## Give a server a region

A server is in a region when its `region` label names it:

```sh
ployz server set web-1 --label-add region=fsn
```

Choose any short name made of letters, digits, hyphens, underscores and dots. A label with other
characters, such as a space or a slash, is not a region, and `ployz server set` and `ployz server
add` refuse it. Servers with the same name are in the same region. Upper and lower case are the
same, so `FSN` is `fsn`.

## A new environment's region

A new environment runs in the region of the first server. A server with no join approval on
record, such as the one that started the cluster, comes first. The others follow in the order you
approved them to join, which can differ from the order they finished joining. When two servers tie, the one with the lowest subnet comes first.
`ployz get @environment.region` shows the region.

This holds however you make the environment: in Ployz Cloud, with `ployz env new` or `ployz up`,
as a new project's first environment, or through the [Ployz agent](../account/agent-sidebar.md).

If any server in the cluster has no region, a new environment gets no region, and its services run
on any server. Give every server a region to pin new environments again. The same goes for the
short time a joining server's details are still reaching the others. An environment you made
before regions also runs on any server, and a branch starts with its parent's region.

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

The rest of the deploy goes ahead, and the service keeps running where it was. The deployment
reads **Deployed, 1 waiting**, and `ployz deploy` exits 3.

The volume stays on its server, so run the service in the volume's region. The deploy prints the
command that does it. With the environment in `fsn` and `db-1` in `syd`:

```sh
ployz set postgres.replicas='{"syd":1}'
ployz deploy
```

The count puts `postgres` alone in `syd`. To move the services that use the environment's region
there too, run `ployz env region syd`. A service with its own counts stays where they put it.

If the volume's server is in no region, the deploy prints the command that gives it the service's
region:

```sh
ployz server set db-1 --label-add region=fsn
ployz deploy
```

## When no server is in the region

If the region's last server loses its label or is removed, the next deploy doesn't start the
environment's services and reports each one:

```
web: not started (no Server in fsn)
```

If the region has servers but none can run services, the line names them and says why:

```
web: not started (fsn-1 is down, and no other Server in fsn is up)
web: not started (fsn-1 takes no services)
```

Replicas that already run keep running. With two services left waiting, the deployment reads
**Nothing deployed, 2 waiting**, and `ployz deploy` exits 3. Give a server the region, bring its
server back, or change the environment's region, and deploy again.

A service with a count for each region starts its replicas in every region that has a server that
can run them. Only the replicas of a region with no such server wait, and the deploy names them
and the regions:

```
api: deployed (1 replica waiting, no Server in syd)
1 deployed · 1 replica waiting · across 2 servers
```

The deployment reads **Deployed, 1 waiting**, and `ployz deploy` exits 3, as it does when a whole
service waits. `ployz deploy --plan` says so first, when no server has the region's label:

```
api → fsn 2 · syd 1: 1 replica waits (no Server in syd)
```

The deploy leaves a replica that already runs in that region as it is. If the deploy changed the
service, that replica stays on its old version until you deploy again with a server there that can
run it:

```
api: deployed (1 replica still on the old version, syd-1 takes no services)
1 deployed · 1 replica on the old version · across 3 servers
```

Give a server that region, bring its server back, or turn its services back on, and deploy again:
the waiting replicas start and the old ones are replaced. A service whose counts are all in
regions with no server isn't started, as above.

A smaller count still removes replicas there: going from `{"fsn":1,"syd":2}` to
`{"fsn":1,"syd":1}` removes one `syd` replica while `syd-1` takes no services. A replica on the
old version goes first.

While a region waits for a replica, the deploy keeps a spare replica running in its place: one on
a server whose `region` label changed, in a region you took out of the counts, or past a smaller
count in another region. So changing a server's label removes nothing, and with the label back
the next deploy changes nothing. A server that joins a region whose count its replicas already
meet doesn't take one from it either. If the deploy changed the service, the spare stays on its
old version, and the deploy says so as above. The spare replica is removed once the waiting region
starts its own.

## Good to know

- **One server in a region holds every replica.** Ployz spreads replicas over the region's
  servers only. With one server there, all of a service's replicas run on it, and they all go
  down with it. Add a second server to the region before you rely on replicas.
- **A service with a volume runs in one region.** Its replicas all write to the volume, which is
  on one server. Ployz refuses counts in two regions for it.
- **Services that share a volume with shared writes run where it is.** When they're kept in
  different regions, the services in the volume's region deploy. Each service in another region
  isn't started, and its line says where the volume is. The volume stays with the services that
  run, so deploying again doesn't start it, and the deployment reads **Failed**. Deploying that
  service by name is refused the same way. So is a service whose own region has no server: a
  server added there wouldn't have the volume. Change where the service runs:

  ```
  worker: not started (pg-data is in fsn, and worker runs in syd. Run worker in fsn instead)
  ```

  When the volume is on a server in no region, or isn't made yet, none of the services that share
  it is started:

  ```
  worker: not started (pg-data is shared by api in fsn and worker in syd. Run them in one region, or give each its own Volume)
  ```

  A service can run in neither region when it shares one volume with a service in `fsn` and
  another with a service in `syd`. Run the services that share them in one region, or split the
  service in two:

  ```
  worker: not started (logs is in syd and pg-data is in fsn, and worker runs in one region. Run the Services that share them in one region, or split worker)
  ```

  A volume that no service kept in another region uses, on a server outside the region, leaves its
  service waiting [as above](#when-a-volume-is-outside-the-region). A service that names no region
  never causes the refusal. The services it shares another volume with deploy meanwhile.
- **A preview environment runs its one replica in the region with the most replicas**: `syd` for
  `{"fsn":1,"syd":3}`. When regions tie, it is the first by name.
- **A drain keeps a replica in its region when the service has a count for that region.** The
  replica moves to another server there, or stays when there is none. If its server's region is
  no longer one the service has a count for, it moves to a server in any region that is. A
  service with one number moves within the environment's region, or to any server when the
  environment has none. A replica a deploy left on its old version, in a region with no server
  that can run it, doesn't stop a drain from moving the service's other replicas.
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
