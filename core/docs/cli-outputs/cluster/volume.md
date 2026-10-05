### volume add --docker on a 2-replica Service (refused)

```console
$ ployz volume add data --docker --mount web:/data
# exit 1
```

stderr:
```
WARNING: Docker volume (not recommended): no size limit, and it stays out of backups and Server moves as they arrive.
data is attached to web; set replicas to 1, or allow shared writes: ployz volume set data --shared-writes
next: ployz volume set data --shared-writes --env production --project shop
```

### volume add --docker

```console
$ ployz volume add data --docker --mount api:/data
# exit 0
```

stdout:
```
Staged new Volume data in shop/production (revision 11).
Storage: Docker volume (no size limit)
```

stderr:
```
WARNING: Docker volume (not recommended): no size limit, and it stays out of backups and Server moves as they arrive.
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
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 12
  },
  "next": "ployz diff",
  "staged": [
    "volumes.cache",
    "api.mounts.cache"
  ],
  "volume": {
    "id": "b403d017-1d63-491c-8264-19180119b004",
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
WARNING: No Server here can host Managed volumes yet, so a Deploy of this one fails until one can: add a Server with Managed volumes, or use --docker instead.
```

### deploy with a Managed volume and no ZFS Server (fails)

```console
$ ployz deploy
# exit 3
```

stdout:
```
queued: web pending, api pending, cache pending, data pending
failed: web not attempted, api not attempted, cache not attempted, data not attempted
Deployment #10 of shop/production: failed
  Service api: No available Server can host Managed volumes. Add a Server with Managed volumes, or keep this data in a Docker volume instead (ployz volume add NAME --docker)
  web: not attempted
  api: not attempted
  cache: not attempted
  data: not attempted
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
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 12
  },
  "volumes": [
    {
      "change": "create",
      "deployed": false,
      "id": "b403d017-1d63-491c-8264-19180119b004",
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
      "id": "fb5fbf41-f4eb-4151-a3b5-2158bb829dc4",
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
Volume data (fb5fbf41-f4eb-4151-a3b5-2158bb829dc4)
Storage: Docker volume (no size limit)
Storage settings: locked after deployment was requested
Shared writes: off
Deployed: no
Next Deploy: create
Mounted by api at /data
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
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 12
  },
  "id": "fb5fbf41-f4eb-4151-a3b5-2158bb829dc4",
  "lineage": "fb5fbf41-f4eb-4151-a3b5-2158bb829dc4",
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
WARNING: Docker volume (not recommended): no size limit, and it stays out of backups and Server moves as they arrive.
Volume cache storage cannot change after deployment has been requested
```

### volume rename

```console
$ ployz volume rename data store
# exit 0
```

stdout:
```
Staged rename of Volume store in shop/production (revision 14).
```

### volume rm (never-deployed Managed volume)

```console
$ ployz volume rm cache
# exit 0
```

stdout:
```
Staged removal of Volume cache in shop/production (revision 15).
```

### deploy with volumes

```console
$ ployz deploy
# exit 0
```

stdout:
```
queued: web pending, api pending, store pending
running: web unchanged, api pending, store pending
applied: web unchanged, api deployed, store deployed
Deployment #11 of shop/production: applied
  web: unchanged
  api: deployed
  store: deployed
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
Staged removal of Volume store in shop/production (revision 16).
```

### deploy refused: volume loss

```console
$ ployz deploy
# exit 1
```

stderr:
```
This permanently deletes the data of store. Accept each by name.
Retry: ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3d1ac37da6e0cfb1
```

### deploy refused: volume loss (--json)

```console
$ ployz deploy --json
# exit 1
```

stdout:
```
{"error":{"code":"confirmation_required","details":{"accept":["store"],"next":"ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3d1ac37da6e0cfb1","version":"16:7:0.10:3d1ac37da6e0cfb1","volumes":[{"deletes":[{"machine_id":"d7bb999f0d454dbcacc63d085021de34","name":"shop-production_vol-fb5fbf41-f4eb-4151-a3b5-2158bb829dc4"}],"docker_volume":"shop-production_vol-fb5fbf41-f4eb-4151-a3b5-2158bb829dc4","id":"fb5fbf41-f4eb-4151-a3b5-2158bb829dc4","name":"store"}]},"message":"This permanently deletes the data of store. Accept each by name.\nRetry: ployz deploy --accept-volume-loss store --expect-version 16:7:0.10:3d1ac37da6e0cfb1"}}
```

### volume inspect unknown

```console
$ ployz volume inspect nope
# exit 1
```

stderr:
```
No Volume named nope in Environment production
```

### build without a Cloud Build Grant

```console
$ ployz build --grant x --deployment 1 --commit abc --fingerprint f
# exit 1
```

stderr:
```
invalid Build Grant "[redacted]": `ployzgrant1:` followed by unpadded base64url of a 64-byte body
```

