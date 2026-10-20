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
error: This permanently deletes the data of store. Accept each by name
retry: ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3db84a7b2f1973e4
```

### deploy refused: volume loss (--json)

```console
$ ployz deploy --json
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"confirmation_required","details":{"accept":["store"],"retry":"ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3db84a7b2f1973e4","version":"16:7:0.10:3db84a7b2f1973e4","volumes":[{"deletes":[{"machine_id":"91e8bbd81cd34c59a186d49d3375bbe8","name":"shop-production_vol-708d0431-a823-4abd-bace-7dd1461c5d21"}],"docker_volume":"shop-production_vol-708d0431-a823-4abd-bace-7dd1461c5d21","id":"708d0431-a823-4abd-bace-7dd1461c5d21","name":"store"}]},"message":"This permanently deletes the data of store. Accept each by name"}}
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

