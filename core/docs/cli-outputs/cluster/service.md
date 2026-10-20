### service ls

```console
$ ployz service ls
# exit 0
```

stdout:
```
SERVICE	PRIVATE DNS	SOURCE	NEXT DEPLOY
api	api	image	-
web	web	image	-
```

### service inspect

```console
$ ployz service inspect web
# exit 0
```

stdout:
```
service = web
private dns = web
source = image
cpuLimit = - (default)
healthcheck = - (default)
image = "alpine:3.23.3" (default -)
maxRetries = 10 (default)
memLimit = - (default)
preDeployCommand = - (default)
privateDns = "web" (default)
registryCredential = - (default)
replicas = 2 (default 1)
restartPolicy = "unless-stopped" (default)
startCommand = "sh -c 'echo started; while true; do echo tick; sleep 2; done'" (default -)
template = - (default)
updateOrder = - (default)
env.GREETING = "hello"
env.GREETING.exported = false (default)
domains = -
```

### service inspect --json

```console
$ ployz service inspect web --json
# exit 0
```

stdout:
```
{
  "change": null,
  "changes": [],
  "domains": [],
  "environment": {
    "id": "96430fcc-088f-45be-9cbe-5e67e7e2f507",
    "name": "production",
    "project": "shop",
    "revision": 10
  },
  "id": "608e4644-cd97-46d2-bb82-416f75554478",
  "lineage": "608e4644-cd97-46d2-bb82-416f75554478",
  "name": "web",
  "private_dns": "web",
  "row": "608e4644-cd97-46d2-bb82-416f75554478:node",
  "settings": [
    {
      "apply": "staged",
      "default": null,
      "path": "web.cpuLimit",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.healthcheck",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.image",
      "value": "alpine:3.23.3"
    },
    {
      "apply": "staged",
      "default": 10,
      "path": "web.maxRetries",
      "value": 10
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.memLimit",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.preDeployCommand",
      "value": null
    },
    {
      "apply": "staged",
      "default": "web",
      "path": "web.privateDns",
      "value": "web"
    },
    {
      "apply": "immediate",
      "default": null,
      "path": "web.registryCredential",
      "value": null
    },
    {
      "apply": "staged",
      "default": 1,
      "path": "web.replicas",
      "value": 2
    },
    {
      "apply": "staged",
      "default": "unless-stopped",
      "path": "web.restartPolicy",
      "value": "unless-stopped"
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.startCommand",
      "value": "sh -c 'echo started; while true; do echo tick; sleep 2; done'"
    },
    {
      "apply": "immediate",
      "default": null,
      "path": "web.template",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.updateOrder",
      "value": null
    },
    {
      "apply": "staged",
      "default": null,
      "path": "web.env.GREETING",
      "row": "608e4644-cd97-46d2-bb82-416f75554478:variables.GREETING",
      "value": "hello"
    },
    {
      "apply": "staged",
      "default": false,
      "path": "web.env.GREETING.exported",
      "value": false
    }
  ],
  "source": "image",
  "template": null,
  "values": {
    "env": {
      "GREETING": "hello"
    },
    "image": "alpine:3.23.3",
    "maxRetries": 10,
    "privateDns": "web",
    "replicas": 2,
    "restartPolicy": "unless-stopped",
    "startCommand": "sh -c 'echo started; while true; do echo tick; sleep 2; done'"
  }
}
```

### service stop

```console
$ ployz service stop api
# exit 0
```

stdout:
```
Stopped shop-production/api on beta.
```

### service start

```console
$ ployz service start api
# exit 0
```

stdout:
```
Started shop-production/api on beta.
```

### service restart

```console
$ ployz service restart web
# exit 0
```

stdout:
```
Restarted shop-production/web on alpha.
Restarted shop-production/web on gamma.
```

### service restart --json

```console
$ ployz service restart api --json
# exit 0
```

stdout:
```
{
  "container_failures": [],
  "containers": [
    {
      "action": "restart",
      "container_id": "b6ac931643a597b358fc657833d6e1d6f181f6d5daeee5e6da5ed777c1e2ed3c",
      "machine_id": "7f00d39ce93641bd8c5fdd7d91395aa7",
      "service": "shop-production/api"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
Restarted shop-production/api on beta.
```

### service stop unknown

```console
$ ployz service stop nope
# exit 1
```

stderr:
```
error: No Service named nope in Environment production
valid: api, web
```

### service port-forward (killed after 4s)

```console
$ ployz service port-forward web 0:80
```

stdout:
```
Forwarding 127.0.0.1:37333 to shop-production/web (ployz-create-14f3ee91f0cdcf386f41ca376f50035a34f5bd11090b6c5bebea4ed84260647d) port 80.
```

stderr:
```
Ctrl-C stops it.
# exit 124
```

### service port-forward --json (killed after 4s)

stdout:
```
{"container":"0de25c6f017ea7ebd6e2e707692194812cd00af850617f2469de5fe502f0efcd","local":"127.0.0.1:38473","remote":"10.210.0.3:80","service":"shop-production/web"}
```

stderr:
```
# exit 124
```

