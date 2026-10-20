# Ployz Cloud CLI outputs: domain

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## domain (seed: acme.com on web in Project shop, set by PLOYZ_PROJECT; hosted DNS is dead in verify)

### domain ls

```console
$ ployz domain ls
# exit 0
```

stdout:
```
DOMAIN	SERVICE	PORT	STATUS	REASON
acme.com	web	-	setting up	Cloud can't see the Servers right now
```

### domain ls --json

```console
$ ployz domain ls --json
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
      "reason": "Cloud can't see the Servers right now",
      "service": "web",
      "status": "setting_up"
    }
  ],
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 13
  }
}
```

### domain add api (generated)

```console
$ ployz domain add api
# exit 0
```

stdout:
```
Staged domain api.ada.ployz.test on api in shop/production.
```

stderr:
```
next: ployz deploy --project shop
```

### domain add worker --json (generated)

```console
$ ployz domain add worker --json
# exit 0
```

stdout:
```
{
  "added": true,
  "domain": {
    "hostname": "worker.ada.ployz.test",
    "kind": "generated",
    "port": null,
    "prefix": "worker",
    "service": "worker"
  },
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 15
  },
  "free_prefix": null,
  "next": "ployz deploy --project shop",
  "staged": [
    "worker"
  ]
}
```

### domain add web shop.example.org (custom)

```console
$ ployz domain add web shop.example.org
# exit 0
```

stdout:
```
Staged domain shop.example.org on web in shop/production.
```

stderr:
```
next: ployz deploy --project shop
```

### domain add web docs.example.org --port 8080 --json

```console
$ ployz domain add web docs.example.org --port 8080 --json
# exit 0
```

stdout:
```
{
  "added": true,
  "domain": {
    "hostname": "docs.example.org",
    "kind": "custom",
    "port": 8080,
    "service": "web"
  },
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 17
  },
  "free_prefix": null,
  "next": "ployz deploy --project shop",
  "staged": [
    "web"
  ]
}
```

### domain add, duplicate host

```console
$ ployz domain add web shop.example.org
# exit 0
```

stdout:
```
Nothing changed in shop/production; shop.example.org is already on web.
```

### domain add, unknown service

```console
$ ployz domain add nosuch
# exit 1
```

stderr:
```
error: No Service named nosuch in Environment production
valid: web, api, postgres, worker, redis
```

### domain add, unknown service --json

```console
$ ployz domain add nosuch --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["web","api","postgres","worker","redis"]},"message":"No Service named nosuch in Environment production"}}
```

### domain add, bad host

```console
$ ployz domain add web not\ a\ host
# exit 2
```

stderr:
```
error: Expected a hostname like app.example.com
```

### domain set api myapi

```console
$ ployz domain set api myapi
# exit 0
```

stdout:
```
Staged generated domain myapi.ada.ployz.test on api in shop/production.
```

stderr:
```
next: ployz deploy --project shop
```

### domain set worker myworker --json

```console
$ ployz domain set worker myworker --json
# exit 0
```

stdout:
```
{
  "domain": {
    "hostname": "myworker.ada.ployz.test",
    "kind": "generated",
    "port": null,
    "prefix": "myworker",
    "service": "worker"
  },
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 19
  },
  "next": "ployz deploy --project shop",
  "staged": [
    "worker"
  ]
}
```

### domain set, prefix with a dot

```console
$ ployz domain set api my.api
# exit 2
```

stderr:
```
error: Expected one DNS label, like shop
```

### domain ls after changes

```console
$ ployz domain ls
# exit 0
```

stdout:
```
DOMAIN	SERVICE	PORT	STATUS	REASON
acme.com	web	-	setting up	Cloud can't see the Servers right now
shop.example.org	web	-	setting up	Live after your next deploy
docs.example.org	web	8080	setting up	Live after your next deploy
myapi.ada.ployz.test	api	-	setting up	Live after your next deploy
myworker.ada.ployz.test	worker	-	setting up	Live after your next deploy
```

stderr:
```
next: ployz deploy --project shop
```

### domain check acme.com

```console
$ ployz domain check acme.com
# exit 0
```

stdout:
```
domain = acme.com
service = web
port = -
status = setting up
reason = Cloud can't see the Servers right now
```

### domain check acme.com --json

```console
$ ployz domain check acme.com --json
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
    "reason": "Cloud can't see the Servers right now",
    "service": "web",
    "status": "setting_up"
  },
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 19
  }
}
```

### domain check unknown

```console
$ ployz domain check nope.example.org
# exit 1
```

stderr:
```
error: No such domain in Environment production
valid: acme.com, shop.example.org, docs.example.org, myapi.ada.ployz.test, myworker.ada.ployz.test
```

### domain rm shop.example.org

```console
$ ployz domain rm shop.example.org
# exit 0
```

stdout:
```
Staged removal of domain shop.example.org on web in shop/production.
```

stderr:
```
next: ployz deploy --project shop
```

### domain rm docs.example.org --json

```console
$ ployz domain rm docs.example.org --json
# exit 0
```

stdout:
```
{
  "domain": {
    "hostname": "docs.example.org",
    "kind": "custom",
    "port": 8080,
    "service": "web"
  },
  "environment": {
    "id": "2aff4644-5322-460a-97ec-ec5a24b95fef",
    "name": "production",
    "project": "shop",
    "revision": 21
  },
  "next": "ployz deploy --project shop",
  "staged": [
    "web"
  ]
}
```

### domain rm unknown

```console
$ ployz domain rm nope.example.org
# exit 1
```

stderr:
```
error: No such domain in Environment production
valid: acme.com, myapi.ada.ployz.test, myworker.ada.ployz.test
```

### domain rm unknown --json

```console
$ ployz domain rm nope.example.org --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["acme.com","myapi.ada.ployz.test","myworker.ada.ployz.test"]},"message":"No such domain in Environment production"}}
```

### domain ls --project nosuch

```console
$ ployz domain ls --project nosuch
# exit 1
```

stderr:
```
error: No Project named nosuch
```

