### volume add --docker on a 2-replica Service (refused)

```console
$ ployz volume add data --docker --mount web:/data
# exit 1
```

stderr:
```
error: data would be attached to web; set replicas to 1, or add it with shared writes on
next: ployz volume add data --mount web:/data --docker --shared-writes --env production --project shop
```

### volume add --docker

```console
$ ployz volume add data --docker --mount api:/data
# exit 0
```

stdout:
```
Staged new Volume data in shop/production.
Storage: Docker volume (no size limit)
```

stderr:
```
! Docker volume (not recommended): no size limit, and it stays out of backups and Server moves as they arrive.
next: ployz deploy
```

### volume add managed (--json)

```console
$ ployz volume add cache --size 1 --mount api:/cache --json
# exit 0
```

stdout:
```
{
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 12
  },
  "next": "ployz deploy",
  "staged": [
    "volumes.cache",
    "api.mounts.cache"
  ],
  "volume": {
    "id": "39f15369-bfc8-4367-a9da-ae9ec8ff496c",
    "name": "cache",
    "shared_writes": false,
    "storage": {
      "kind": "provisioned",
      "maximumBytes": 1000000000
    }
  },
  "warnings": [
    "No Server here can host Managed volumes yet, so a Deploy of this one fails until one can: add a Server with Managed volumes, or use --docker instead."
  ]
}
```

stderr:
```
! No Server here can host Managed volumes yet, so a Deploy of this one fails until one can: add a Server with Managed volumes, or use --docker instead.
```

### deploy with a Managed volume and no ZFS Server (fails)

```console
$ ployz deploy
# exit 1
```

stderr:
```
Deploying #9 of shop/production
  api: not started
  web: not started
  Volume cache: not attempted
  Volume data: not attempted
───────────────
4 not attempted
error: Service api: No available Server can host Managed volumes. Add a Server with Managed volumes, or keep this data in a Docker volume instead (ployz volume add NAME --docker).
retry: ployz deploy --project shop --env production
```

### volume ls

```console
$ ployz volume ls
# exit 0
```

stdout:
```
VOLUME	STORAGE	SHARED WRITES	MOUNTS	DEPLOYED	NEXT DEPLOY
cache	Managed volume (1 GB limit)	off	api:/cache	no	create
data	Docker volume (no size limit)	off	api:/data	no	create
```

### volume ls --json

```console
$ ployz volume ls --json
# exit 0
```

stdout:
```
{
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 12
  },
  "volumes": [
    {
      "change": "create",
      "deployed": false,
      "id": "39f15369-bfc8-4367-a9da-ae9ec8ff496c",
      "mounts": [
        {
          "path": "/cache",
          "service": "api"
        }
      ],
      "name": "cache",
      "shared_writes": false,
      "storage": {
        "kind": "provisioned",
        "maximumBytes": 1000000000
      },
      "storage_locked": true
    },
    {
      "change": "create",
      "deployed": false,
      "id": "708d0431-a823-4abd-bace-7dd1461c5d21",
      "mounts": [
        {
          "path": "/data",
          "service": "api"
        }
      ],
      "name": "data",
      "shared_writes": false,
      "storage": {
        "kind": "docker"
      },
      "storage_locked": true
    }
  ]
}
```

### volume inspect

```console
$ ployz volume inspect data
# exit 0
```

stdout:
```
volume = data
storage = Docker volume (no size limit)
storage settings = locked after deployment was requested
shared writes = off
deployed = no
next deploy = create
mounted by = api at /data
```

### volume inspect --json

```console
$ ployz volume inspect data --json
# exit 0
```

stdout:
```
{
  "change": "create",
  "deployed": false,
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 12
  },
  "id": "708d0431-a823-4abd-bace-7dd1461c5d21",
  "lineage": "708d0431-a823-4abd-bace-7dd1461c5d21",
  "mounts": [
    {
      "path": "/data",
      "service": "api"
    }
  ],
  "name": "data",
  "shared_writes": false,
  "storage": {
    "kind": "docker"
  },
  "storage_locked": true
}
```

### volume set --shared-writes

```console
$ ployz volume set data --shared-writes
# exit 0
```

stdout:
```
Shared writes on for Volume data in shop/production; this applies now.
```

### volume set --docker (managed -> docker)

```console
$ ployz volume set cache --docker
# exit 1
```

stderr:
```
error: Volume cache keeps its storage once a Deploy has been requested; add a new Volume for other storage
```

### volume rename

```console
$ ployz volume rename data store
# exit 0
```

stdout:
```
Staged rename of Volume store in shop/production.
```

stderr:
```
next: ployz deploy
```

### volume rm (never-deployed Managed volume)

```console
$ ployz volume rm cache
# exit 0
```

stdout:
```
Staged removal of Volume cache in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy with volumes

```console
$ ployz deploy
# exit 0
```

stderr:
```
Deploying #10 of shop/production
  api (depth 0): booting 0/1
  web (depth 0): unchanged
  Volume store: pending
  api (depth 0): switching 1/1
  api (depth 0): switched 1/1
  api: draining old
  Volume store: completed
───────────────────────────────────────────
2 deployed · 1 unchanged · across 2 servers
```

### volume ls after deploy

```console
$ ployz volume ls
# exit 0
```

stdout:
```
VOLUME	STORAGE	SHARED WRITES	MOUNTS	DEPLOYED	NEXT DEPLOY
store	Docker volume (no size limit)	on	api:/data	yes	-
```

### volume rm

```console
$ ployz volume rm store
# exit 0
```

stdout:
```
Staged removal of Volume store in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy refused: volume loss

```console
$ ployz deploy
# exit 2
```

stderr:
```
error: This deletes the data of store. Accept each by name
retry: ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3db84a7b2f1973e4
```

### deploy refused: volume loss (--json)

```console
$ ployz deploy --json
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"confirmation_required","details":{"accept":["store"],"retry":"ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3db84a7b2f1973e4","version":"16:7:0.10:3db84a7b2f1973e4","volumes":[{"deletes":[{"machine_id":"91e8bbd81cd34c59a186d49d3375bbe8","name":"shop-production_vol-708d0431-a823-4abd-bace-7dd1461c5d21"}],"docker_volume":"shop-production_vol-708d0431-a823-4abd-bace-7dd1461c5d21","id":"708d0431-a823-4abd-bace-7dd1461c5d21","name":"store"}]},"message":"This deletes the data of store. Accept each by name"}}
```

### volume inspect unknown

```console
$ ployz volume inspect nope
# exit 1
```

stderr:
```
error: No Volume named nope in Environment production
```

### build without a Cloud Build Grant

```console
$ ployz build --grant x --deployment 1 --commit abc --fingerprint f
# exit 2
```

stderr:
```
error: invalid Build Grant "[redacted]": `ployzgrant1:` followed by unpadded base64url of a 64-byte body
```


### volume trash restore (a Server didn't answer, refused)

```console
$ ployz volume trash restore bxkvrx7hbd5vh4t5te7eggac44 --connect ssh://root@203.0.113.10:19054
# exit 1
```

stderr:
```
error: Restore needs every Server to answer, since a copy on one that didn't would become a second writer. machine-1 did not answer (target Machine RPC timed out)
```

### volume trash ls (an entry being restored)

```console
$ ployz volume trash ls --connect ssh://root@203.0.113.10:19054
# exit 0
```

stdout:
```
SERVER	ID	VOLUME	COPY	SIZE	DELETED	EXPIRES
machine-2	bxkvrx7hbd5vh4t5te7eggac44	verify-production_vol-30d12bce-781b-4275-92f0-f3c7f9827e48	volume (restoring)	0 MB	2026-11-25 14:48 UTC	2026-12-02 14:48 UTC
machine-2	iq6riqmlvqtlhqjfcc24gw3rhy	verify-production_vol-d62533bf-6a2a-47a0-b5c0-d099098f09be	volume	0 MB	2026-11-25 14:48 UTC	2026-12-02 14:48 UTC
```

### volume trash restore

```console
$ ployz volume trash restore bxkvrx7hbd5vh4t5te7eggac44 --connect ssh://root@203.0.113.10:19054
# exit 0
```

stdout:
```
Restored verify-production_vol-30d12bce-781b-4275-92f0-f3c7f9827e48 on machine-2. No service mounts it yet.
```

stderr:
```
In verify/production, redeploy a deployment from before the removal, then discard the removal. Review it with `ployz diff` and save it before any automatic deploy runs, or that deploy deletes the Volume again.
next: ployz deployment redeploy DEPLOYMENT --project verify --env production
next: ployz discard volumes.VOLUME --project verify --env production
next: ployz deploy --skip-deploy --project verify --env production
```

### volume trash restore --json (deleted and restored before)

```console
$ ployz --json volume trash restore 72r44tl6ubpcfn4rx7kjpb7y5i --connect ssh://root@203.0.113.10:19054
# exit 0
```

stdout:
```
{
  "next": [
    "ployz deployment redeploy DEPLOYMENT --project verify --env production",
    "ployz discard volumes.VOLUME --project verify --env production",
    "ployz deploy --skip-deploy --project verify --env production"
  ],
  "restored": {
    "id": "72r44tl6ubpcfn4rx7kjpb7y5i",
    "name": "verify-production_vol-30d12bce-781b-4275-92f0-f3c7f9827e48",
    "place": "root",
    "state": {
      "epoch": 2,
      "kind": "restored"
    }
  },
  "server": "machine-2"
}
```

### volume trash restore (another entry of the Volume being restored, refused)

```console
$ ployz volume trash restore dupdupdupdupdupdupdupdupaa --connect ssh://root@203.0.113.10:19054
# exit 1
```

stderr:
```
error: Volume verify-production_vol-00cdd286-af1a-4d59-8b6e-39a6a2430997 is also being restored on this Server from Trash entry mxcjurpeiue2nrzgf3dvhyovym. Pick the one to keep. Delete the other with `sudo ployz volume trash empty ID`, or stop restoring it and it expires in 7 days.
```

### volume trash restore (an ID not in the Trash, refused)

```console
$ ployz volume trash restore aaaaaaaaaaaaaaaaaaaaaaaaaa --connect ssh://root@203.0.113.10:19054
# exit 1
```

stderr:
```
error: machine-2's Trash holds no aaaaaaaaaaaaaaaaaaaaaaaaaa, and no Volume on machine-2 came from that entry.
inspect: ployz volume ls
```

### volume trash empty (entries being restored stay)

```console
$ ployz volume trash empty
# exit 0
```

stdout:
```
Deleted 1 entry from machine-2's Trash for good.
```

stderr:
```
Delete 1 entry in machine-2's Trash for good? yes
dupdupdupdupdupdupdupdupaa is being restored, so it stays in machine-2's Trash.
mxcjurpeiue2nrzgf3dvhyovym is being restored, so it stays in machine-2's Trash.
```

### volume trash empty --json (only entries being restored)

```console
$ ployz --json volume trash empty
# exit 0
```

stdout:
```
{
  "emptied": {
    "emptied": [],
    "failed": [],
    "missing": [],
    "restoring": [
      "dupdupdupdupdupdupdupdupaa",
      "mxcjurpeiue2nrzgf3dvhyovym"
    ]
  },
  "server": "machine-2"
}
```

### volume trash empty ID (an entry being restored)

```console
$ ployz volume trash empty mxcjurpeiue2nrzgf3dvhyovym
# exit 0
```

stdout:
```
Deleted 1 entry from machine-2's Trash for good.
```
