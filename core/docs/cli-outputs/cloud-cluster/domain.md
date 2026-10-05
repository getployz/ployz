# Domains through Cloud (real Server, fake Hosted DNS)

## ls and check

### domain ls

```console
$ ployz domain ls
# exit 0
```

stdout:
```
acme.com	web → PORT	Needs attention · Port 80 is closed on 203.0.113.10
web.ada.ployz.test	web → 80	Needs attention · Port 80 is closed on 203.0.113.10
```

### domain ls --json

```console
$ ployz --json domain ls
# exit 0
```

stdout:
```
{
  "domains": [
    {
      "action": null,
      "hostname": "acme.com",
      "kind": "custom",
      "port": null,
      "reason": "Port 80 is closed on 203.0.113.10",
      "service": "web",
      "status": "needs_attention"
    },
    {
      "action": null,
      "hostname": "web.ada.ployz.test",
      "kind": "generated",
      "port": 80,
      "prefix": "web",
      "reason": "Port 80 is closed on 203.0.113.10",
      "service": "web",
      "status": "needs_attention"
    }
  ],
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 38
  }
}
```

### domain check the generated domain

```console
$ ployz domain check web.ada.ployz.test
# exit 0
```

stdout:
```
web.ada.ployz.test	web → 80	Needs attention · Port 80 is closed on 203.0.113.10
```

### domain check --json the generated domain

```console
$ ployz --json domain check web.ada.ployz.test
# exit 0
```

stdout:
```
{
  "domain": {
    "action": null,
    "hostname": "web.ada.ployz.test",
    "kind": "generated",
    "port": 80,
    "prefix": "web",
    "reason": "Port 80 is closed on 203.0.113.10",
    "service": "web",
    "status": "needs_attention"
  },
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 38
  }
}
```

### domain check a custom domain

```console
$ ployz domain check acme.com
# exit 0
```

stdout:
```
acme.com	web → PORT	Needs attention · Port 80 is closed on 203.0.113.10
```

### domain check --json a custom domain

```console
$ ployz --json domain check acme.com
# exit 0
```

stdout:
```
{
  "domain": {
    "action": null,
    "hostname": "acme.com",
    "kind": "custom",
    "port": null,
    "reason": "Port 80 is closed on 203.0.113.10",
    "service": "web",
    "status": "needs_attention"
  },
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 38
  }
}
```

## Change the generated domain

### domain set

```console
$ ployz domain set web shopweb
# exit 0
```

stdout:
```
Staged generated domain shopweb.ada.ployz.test on web in shop/production (revision 39).
```

### domain ls with a staged change

```console
$ ployz domain ls
# exit 0
```

stdout:
```
acme.com	web → PORT	Needs attention · Port 80 is closed on 203.0.113.10
shopweb.ada.ployz.test	web → 80	Setting up · Live after your next deploy
```

### diff

```console
$ ployz diff
# exit 0
```

stdout:
```
web (update)
  web.managedHostnames: [{"prefix":"web","targetPort":80}] -> [{"prefix":"shopweb","targetPort":80}]
next: ployz deploy --expect-version 39:27:0.54
```

### deploy

```console
$ ployz deploy
# exit 0
```

stdout:
```
Following Deployment #61; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
applied: web deployed, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #61 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### domain ls after deploy

```console
$ ployz domain ls
# exit 0
```

stdout:
```
acme.com	web → PORT	Needs attention · Port 80 is closed on 203.0.113.10
shopweb.ada.ployz.test	web → 80	Needs attention · Port 80 is closed on 203.0.113.10
```

### domain set --json back

```console
$ ployz --json domain set web web --port 80
# exit 0
```

stdout:
```
{
  "domain": {
    "hostname": "web.ada.ployz.test",
    "kind": "generated",
    "port": 80,
    "prefix": "web",
    "service": "web"
  },
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 40
  },
  "next": "ployz deploy",
  "staged": [
    "web"
  ]
}
```

### deploy

```console
$ ployz deploy
# exit 0
```

stdout:
```
Following Deployment #62; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
applied: web deployed, api unchanged, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #62 of shop/production: applied
  web: deployed
  api: unchanged
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

## Add and remove a generated domain

### domain add a generated domain

```console
$ ployz domain add api
# exit 0
```

stdout:
```
Staged domain api.ada.ployz.test on api in shop/production (revision 41).
```

### deploy

```console
$ ployz deploy
# exit 0
```

stdout:
```
Following Deployment #63; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web unchanged, api pending, postgres unchanged, worker unchanged, pg-data unchanged
applied: web unchanged, api deployed, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #63 of shop/production: applied
  web: unchanged
  api: deployed
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### domain ls with two generated domains

```console
$ ployz domain ls
# exit 0
```

stdout:
```
acme.com	web → PORT	Needs attention · Port 80 is closed on 203.0.113.10
web.ada.ployz.test	web → 80	Needs attention · Port 80 is closed on 203.0.113.10
api.ada.ployz.test	api → PORT	Needs attention · Port 80 is closed on 203.0.113.10
```

### domain rm

```console
$ ployz domain rm api.ada.ployz.test
# exit 0
```

stdout:
```
Staged removal of domain api.ada.ployz.test on api in shop/production (revision 42).
```

### deploy

```console
$ ployz deploy
# exit 0
```

stdout:
```
Following Deployment #64; stopping this leaves it running.
queued: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web pending, api pending, postgres pending, worker pending, pg-data pending
running: web unchanged, api pending, postgres unchanged, worker unchanged, pg-data unchanged
applied: web unchanged, api deployed, postgres unchanged, worker unchanged, pg-data unchanged
Deployment #64 of shop/production: applied
  web: unchanged
  api: deployed
  postgres: unchanged
  worker: unchanged
  pg-data: unchanged
```

### domain add --json a generated domain

```console
$ ployz --json domain add api
# exit 0
```

stdout:
```
{
  "domain": {
    "hostname": "api.ada.ployz.test",
    "kind": "generated",
    "port": null,
    "prefix": "api",
    "service": "api"
  },
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 43
  },
  "next": "ployz deploy",
  "staged": [
    "api"
  ]
}
```

### domain rm --json

```console
$ ployz --json domain rm api.ada.ployz.test
# exit 0
```

stdout:
```
{
  "domain": {
    "hostname": "api.ada.ployz.test",
    "kind": "generated",
    "port": null,
    "prefix": "api",
    "service": "api"
  },
  "environment": {
    "id": "4b4e5354-8834-4aea-a807-e65f832680bc",
    "name": "production",
    "project": "shop",
    "revision": 44
  },
  "next": "ployz deploy",
  "staged": [
    "api"
  ]
}
```

### discard

```console
$ ployz discard
# exit 0
```

stdout:
```
Discarded every staged change in shop/production (revision 44).
```

## Remove a custom domain, then discard

### domain rm a custom domain

```console
$ ployz domain rm acme.com
# exit 0
```

stdout:
```
Staged removal of domain acme.com on web in shop/production (revision 45).
```

### domain ls with a staged removal

```console
$ ployz domain ls
# exit 0
```

stdout:
```
web.ada.ployz.test	web → 80	Needs attention · Port 80 is closed on 203.0.113.10
```

### discard

```console
$ ployz discard
# exit 0
```

stdout:
```
Discarded every staged change in shop/production (revision 46).
```

### domain add a custom domain that exists

```console
$ ployz domain add web acme.com
# exit 0
```

stdout:
```
Staged domain acme.com on web in shop/production (revision 46).
```

