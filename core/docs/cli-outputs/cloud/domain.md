# Ployz Cloud CLI outputs: domain

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## domain (seed: acme.com on web; hosted DNS is dead in verify)

### domain ls

```console
$ ployz domain ls
# exit 0
```

stdout:
```
acme.com	web → PORT	Setting up · Deploying
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
      "reason": "Deploying",
      "service": "web",
      "status": "setting_up"
    }
  ],
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 9
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
Staged domain api on api in shop/production (revision 10).
```

### domain add worker --json (generated)

```console
$ ployz domain add worker --json
# exit 0
```

stdout:
```
{
  "domain": {
    "hostname": null,
    "kind": "generated",
    "port": null,
    "prefix": "worker",
    "service": "worker"
  },
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 11
  },
  "next": "ployz deploy",
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
Staged domain shop.example.org on web in shop/production (revision 12).
```

### domain add web docs.example.org --port 8080 --json

```console
$ ployz domain add web docs.example.org --port 8080 --json
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
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 13
  },
  "next": "ployz deploy",
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
Staged domain shop.example.org on web in shop/production (revision 13).
```

### domain add, unknown service

```console
$ ployz domain add nosuch
# exit 1
```

stderr:
```
No Service named nosuch in Environment production
valid: web, api, postgres, worker
```

### domain add, unknown service --json

```console
$ ployz domain add nosuch --json
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"did_you_mean":null,"valid_children":["web","api","postgres","worker"]},"message":"No Service named nosuch in Environment production"}}
```

### domain add, bad host

```console
$ ployz domain add web not\ a\ host
# exit 2
```

stderr:
```
Expected a hostname like app.example.com
```

### domain set api myapi

```console
$ ployz domain set api myapi
# exit 0
```

stdout:
```
Staged generated domain myapi on api in shop/production (revision 14).
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
    "hostname": null,
    "kind": "generated",
    "port": null,
    "prefix": "myworker",
    "service": "worker"
  },
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 15
  },
  "next": "ployz deploy",
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
Expected one DNS label, like shop
```

### domain ls after changes

```console
$ ployz domain ls
# exit 0
```

stdout:
```
acme.com	web → PORT	Setting up · Deploying
shop.example.org	web → PORT	Setting up · Live after your next deploy
docs.example.org	web → 8080	Setting up · Live after your next deploy
myapi	api → PORT	Setting up · Live after your next deploy
myworker	worker → PORT	Setting up · Live after your next deploy
```

### domain check acme.com

```console
$ ployz domain check acme.com
# exit 0
```

stdout:
```
acme.com	web → PORT	Setting up · Deploying
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
    "reason": "Deploying",
    "service": "web",
    "status": "setting_up"
  },
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 15
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
No such domain in Environment production
valid: acme.com, shop.example.org, docs.example.org, myapi, myworker
```

### domain rm shop.example.org

```console
$ ployz domain rm shop.example.org
# exit 0
```

stdout:
```
Staged removal of domain shop.example.org on web in shop/production (revision 16).
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
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 17
  },
  "next": "ployz deploy",
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
No such domain in Environment production
valid: acme.com, myapi, myworker
```

### domain rm unknown --json

```console
$ ployz domain rm nope.example.org --json
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"valid_children":["acme.com","myapi","myworker"]},"message":"No such domain in Environment production"}}
```

### domain ls --project nosuch

```console
$ ployz domain ls --project nosuch
# exit 1
```

stderr:
```
No Project named nosuch
```

