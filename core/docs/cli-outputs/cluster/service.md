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
Service web (a085f2f9-e4df-439e-9d34-99e5b3ee7405)
Private DNS: web
Source: image
env={"GREETING":"hello"}
image="alpine:3.23.3"
maxRetries=10
privateDns="web"
replicas=2
restartPolicy="unless-stopped"
startCommand="sh -c 'echo started; while true; do echo tick; sleep 2; done'"
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
  "environment": {
    "id": "52712cb1-5621-43aa-987b-900e655738a0",
    "name": "production",
    "project": "shop",
    "revision": 10
  },
  "id": "a085f2f9-e4df-439e-9d34-99e5b3ee7405",
  "lineage": "a085f2f9-e4df-439e-9d34-99e5b3ee7405",
  "name": "web",
  "private_dns": "web",
  "row": "a085f2f9-e4df-439e-9d34-99e5b3ee7405:node",
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
stop	shop-production/api	3ba1dcbd6b854ea699b76f6ef296576d	01479b917a80712965fb38a499c12d1ad0320cd1f48a628d7bccbdbcd8b0d39f
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### service start

```console
$ ployz service start api
# exit 0
```

stdout:
```
start	shop-production/api	3ba1dcbd6b854ea699b76f6ef296576d	01479b917a80712965fb38a499c12d1ad0320cd1f48a628d7bccbdbcd8b0d39f
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### service restart

```console
$ ployz service restart web
# exit 0
```

stdout:
```
stop	shop-production/web	0523a6681d894d128f69ea28b7ed62d5	24674546193d6f0d79b6a3a6617d8157f0a6cab24ea21a995302b4e2986251bb
start	shop-production/web	0523a6681d894d128f69ea28b7ed62d5	24674546193d6f0d79b6a3a6617d8157f0a6cab24ea21a995302b4e2986251bb
stop	shop-production/web	d7bb999f0d454dbcacc63d085021de34	e7023f44bd985200f7822f44a7f596008fa76e14e01a94449b790961f9bc3ac9
start	shop-production/web	d7bb999f0d454dbcacc63d085021de34	e7023f44bd985200f7822f44a7f596008fa76e14e01a94449b790961f9bc3ac9
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
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
      "action": "stop",
      "container_id": "01479b917a80712965fb38a499c12d1ad0320cd1f48a628d7bccbdbcd8b0d39f",
      "machine_id": "3ba1dcbd6b854ea699b76f6ef296576d",
      "service": "shop-production/api"
    },
    {
      "action": "start",
      "container_id": "01479b917a80712965fb38a499c12d1ad0320cd1f48a628d7bccbdbcd8b0d39f",
      "machine_id": "3ba1dcbd6b854ea699b76f6ef296576d",
      "service": "shop-production/api"
    }
  ],
  "failures": [],
  "omitted": []
}
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
stop	shop-production/api	3ba1dcbd6b854ea699b76f6ef296576d	01479b917a80712965fb38a499c12d1ad0320cd1f48a628d7bccbdbcd8b0d39f
start	shop-production/api	3ba1dcbd6b854ea699b76f6ef296576d	01479b917a80712965fb38a499c12d1ad0320cd1f48a628d7bccbdbcd8b0d39f
```

### service stop unknown

```console
$ ployz service stop nope
# exit 1
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
No running Service "nope"; ployz ps lists what's running
```

### service port-forward (killed after 4s)

```console
$ ployz service port-forward web 0:80
```

stdout:
```
127.0.0.1:36547 -> 10.210.2.3:80 (shop-production/web/24674546193d6f0d79b6a3a6617d8157f0a6cab24ea21a995302b4e2986251bb); Ctrl-C stops
```

stderr:
```
# exit 124
```

### service port-forward --json (killed after 4s)

stdout:
```
{"container":"24674546193d6f0d79b6a3a6617d8157f0a6cab24ea21a995302b4e2986251bb","local":"127.0.0.1:37867","remote":"10.210.2.3:80","service":"shop-production/web"}
```

stderr:
```
# exit 124
```

