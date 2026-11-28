---
title: Regions
description: Keep an environment's services on the servers in one region, such as the ones nearest your users.
---

A region is a name you give your servers, such as `fsn` for Falkenstein or `syd` for Sydney. Give
an environment a region, and its services run on the servers that carry that name, unless a service
has [its own counts](#run-a-service-in-more-than-one-region). Without a region, its services run
on any server.

You set a server's region from the CLI and from the Servers page in Ployz Cloud. You set an
environment's region, and a service's replicas in each region, from the CLI and from Ployz Cloud's
settings.

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

In Ployz Cloud, open the environment's **Settings → Environment** and pick a region under
**Region → Runs in**. The list holds **Any region** and each of your regions with its servers, as in
`Falkenstein · web-1, web-2`. A region no server is in reads `Sydney · no Servers`, and you can't
pick it.

The region is a staged change like any other: `ployz diff` shows it as `@environment.region`,
and `ployz discard @environment.region` drops it. In Ployz Cloud, the environment's bottom bar
counts it and **Details** lists it as **Region**, where you can discard it. To deploy it, run
`ployz deploy` on an approved device: your servers refuse **Deploy** in the dashboard in this
version of Ployz, see [Deploy from the dashboard](../deploy/deployments.md#deploy-from-the-dashboard). The
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

In Ployz Cloud, open the service's **Settings** and find **Scale**. **Replicas** has a row for each
region, with a count you raise and lower. A service with one number shows one row, in the
environment's region, or in **Any region** when the environment has none. **Add region** adds a row
in a region that has none yet, and the **×** on a row takes it out. The counts add up to at most 50.

A service with counts by region keeps them when you change a count, also with one row left, as
`{"fsn":3}`. That holds when it was deployed with them, and when they were set with `ployz set`
before you opened it. It stays in `fsn` when the environment's region changes. To make it follow
the environment again, set one number with `ployz set api.replicas=3`.

Setting the counts back to what's deployed puts back what's deployed, when both the regions and the
counts match. Say api is deployed with one number, 1, in an environment in `fsn`. If `ployz set`
changes it to `{"fsn":3}`, lowering it to 1 puts back the one number, and api follows the
environment's region again. If it was set to `{"syd":3}`, lowering it to 1 keeps `{"syd":1}`. To
follow the environment from there, set one number with `ployz set api.replicas=1`.

A service deployed with one number goes back to one number when you remove the regions you added.
With the count unchanged, nothing is left to deploy.

**Add region** shows only when the environment has a region and the service has no
[volume](volumes.md). A service whose volume doesn't allow shared writes also keeps one replica,
and says **Limited to 1**. You can still pick its region. A service that shares its volume with
another service can only pick the volume's region, and its row says so.

**Details** lists a change as `fsn 2 → fsn 2 · syd 1`, and so does **Deployments → Versions**.

## When a volume is outside the region

A service with a [volume](volumes.md) runs on the server that writes the volume. A
[mirror](mirror-a-volume.md) in the region doesn't count. A deploy doesn't move a volume, so a
service whose volume is outside its region waits. With the environment moved to `syd` and
`pg-data` on a server in `fsn`:

```
postgres: not started (its Volume pg-data is in fsn. Move it with ployz volume move pg-data --to <a server in syd>, or run it in fsn)
```

The rest of the deploy goes ahead. If `postgres` already ran, it keeps running in `fsn`. A new
service doesn't start. The deployment reads
**Deployed, 1 waiting**, or **Nothing deployed, 1 waiting** when no other service had a change,
and `ployz deploy` exits 3. In Ployz Cloud the Deployment page says the same under the
deployment's title, and **Details** says it before you deploy.

Where the approval has an approved version to compare with, such as the Deploy sheet in Ployz
Cloud, it reads "moves pg-data to syd". Once the deploy is approved, move the volume to a server in
`syd`. The move needs no approval of its own, and starts `postgres` there:

```sh
ployz volume move pg-data --to db-syd-1
```

Or run the service in the volume's region, from **Scale** in Ployz Cloud. The deploy
prints the command that does it:

```sh
ployz set postgres.replicas='{"fsn":1}'
ployz deploy
```

The count keeps `postgres` alone in `fsn`, while the environment's other services run in `syd`.
To move the environment back as well, run `ployz env region fsn`.

If the volume's server is in no region, the line names the server, and the deploy prints the
command that gives it the service's region:

```
postgres: not started (its Volume pg-data is on db-1, a Server in no region. Move it with ployz volume move pg-data --to <a server in syd>, or give db-1 the region syd)
```

```sh
ployz server set db-1 --label-add region=syd
ployz deploy
```

**Details** names a service in a region no server is in the same way: `web waits: no Server in fsn`.

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
count in another region. So changing a server's label removes nothing. With the label back, the
spare counts for its region again, and the next deploy spreads the other regions' replicas across
their servers as usual. A server that joins a region whose count its replicas already
meet doesn't take one from it either. If the deploy changed the service, the spare stays on its
old version, and the deploy says so as above. The spare replica is removed once the waiting region
starts its own.

## Good to know

- **One server in a region holds every replica.** Ployz spreads replicas over the region's
  servers only. With one server there, all of a service's replicas run on it, and they all go
  down with it. Add a second server to the region before you rely on replicas.
- **A service with a volume runs in one region.** Its replicas all write to the volume, which is
  on one server. Ployz refuses counts in two regions for it.
- **Services that share a volume run in the same region.** A volume is on one server, so every
  service that mounts it has to run in that server's region. In Ployz Cloud, **Scale** offers such
  a service only that region, and says why: `Must run in fsn. It shares the Volume pg-data with
  postgres, and pg-data is in fsn.` While the volume has no region, because it isn't made yet or
  its server has none, **Scale** holds them to the region they're all set to. When they're set to
  different regions, **Details** says what to change. In **Mount on a service**, a service set to a
  region other than the volume's reads `Must run in fsn`, whatever the others are set to. Pick it
  to read what to change first: `To mount pg-data, set extra to fsn`. Once extra is set to fsn,
  the form lets you mount it. When the environment's
  region changes and one of them has its own region, **Details** says so before you deploy.

  Set apart with `ployz set` or by an agent, the services in the volume's region deploy. Each
  service in another region isn't started, and its line says which region to set. Deploying again
  doesn't start it, and the deployment reads **Failed**. Deploying that service by name is
  refused the same way. So is a service whose own region has no server: a server added there
  wouldn't have the volume.

  ```
  reader: not started (reader and postgres share the Volume pg-data, so they have to run in the same region. pg-data is in fsn and reader is set to syd. Set reader to fsn)
  ```

  When the volume is on a server in no region, none of the services that share it is started, and
  each line gives the same way out, which the deployment's page says once:

  ```
  postgres: not started (postgres and reader share the Volume pg-data, so they have to run in the same region. pg-data is on machine-1, which has no region. Give machine-1 a region, then set them all to it)
  reader: not started (postgres and reader share the Volume pg-data, so they have to run in the same region. pg-data is on machine-1, which has no region. Give machine-1 a region, then set them all to it)
  ```

  When the volume isn't made yet, none of them is started either. Each line says where each
  service is set, and ends with `Set them all to one region`.

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
- **A drain moves a service's volume only to a region the service may run in.** It prefers a
  server in the drained server's region. When the volume has a mirror on a server outside those
  regions, the service stays and the drain says why.
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
