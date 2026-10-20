# Domains through Cloud (real Server, fake Hosted DNS)

## ls and check

### domain ls

```console
$ ployz domain ls
# exit 0
```

stdout:
```
DOMAIN	SERVICE	PORT	STATUS	REASON
acme.com	web	-	needs attention	DNS points to another server
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
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
      "action": {
        "records": [
          {
            "name": "@",
            "type": "AAAA",
            "value": "2001:db8::10"
          }
        ],
        "type": "dns"
      },
      "hostname": "acme.com",
      "kind": "custom",
      "port": null,
      "reason": "DNS points to another server",
      "records": [
        {
          "name": "@",
          "type": "AAAA",
          "value": "2001:db8::10"
        }
      ],
      "service": "web",
      "status": "needs_attention"
    }
  ],
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 26
  }
}
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
```

### domain check the generated domain

```console
$ ployz domain check web.ada.ployz.test
# exit 1
```

stderr:
```
error: No such domain in Environment production
valid: acme.com
```

### domain check --json the generated domain

```console
$ ployz --json domain check web.ada.ployz.test
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["acme.com"]},"message":"No such domain in Environment production"}}
```

### domain check a custom domain

```console
$ ployz domain check acme.com
# exit 0
```

stdout:
```
domain = acme.com
service = web
port = -
status = needs attention
reason = DNS points to another server
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
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
    "action": {
      "records": [
        {
          "name": "@",
          "type": "AAAA",
          "value": "2001:db8::10"
        }
      ],
      "type": "dns"
    },
    "hostname": "acme.com",
    "kind": "custom",
    "port": null,
    "reason": "DNS points to another server",
    "records": [
      {
        "name": "@",
        "type": "AAAA",
        "value": "2001:db8::10"
      }
    ],
    "service": "web",
    "status": "needs_attention"
  },
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 26
  }
}
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
```

## Change the generated domain

### domain set

```console
$ ployz domain set web shopweb
# exit 1
```

stderr:
```
error: web has no generated domain to change
next: ployz domain add web
```

### domain ls with a staged change

```console
$ ployz domain ls
# exit 0
```

stdout:
```
DOMAIN	SERVICE	PORT	STATUS	REASON
acme.com	web	-	needs attention	DNS points to another server
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
```

### diff

```console
$ ployz diff
# exit 0
```

stdout:
```
sentry/config.yml references redis, which was removed; deploys skip it until you change it: ployz config put sentry config.yml --from PATH
```

stderr:
```
No staged changes in shop/production.
```

### deploy

```console
$ ployz deploy
# exit 0
```

stderr:
```
Nothing changed on the Servers; Deployment #29 shipped version #13.
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### domain ls after deploy

```console
$ ployz domain ls
# exit 0
```

stdout:
```
DOMAIN	SERVICE	PORT	STATUS	REASON
acme.com	web	-	needs attention	DNS points to another server
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
```

### domain set --json back

```console
$ ployz --json domain set web web --port 80
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"next":"ployz domain add web"},"message":"web has no generated domain to change"}}
```

### deploy

```console
$ ployz deploy
# exit 0
```

stderr:
```
Nothing changed on the Servers; Deployment #30 shipped version #13.
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

## Add and remove a generated domain

### domain add a generated domain

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
next: ployz deploy
```

### deploy

```console
$ ployz deploy
# exit 0
```

stderr:
```
Deploying #31 of shop/production
  api (depth 0): waiting 0/1
  postgres (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: unchanged
! api.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
! api's image traefik/whoami:v1.11.0 exposes 80; its domains send to 8080. Set the domain port or PORT.
  api (depth 0): booting 0/1
  api (depth 0): switching 1/1
  api (depth 0): switched 1/1
──────────────────────────────────────────
1 deployed · 5 unchanged · across 1 server
! Left PLOYZ_PRIVATE_DOMAIN empty in sentry/config.yml: it is a broken reference.
! Left PORT empty in sentry/config.yml: it is a broken reference.
```

### domain ls with two generated domains

```console
$ ployz domain ls
# exit 0
```

stdout:
```
DOMAIN	SERVICE	PORT	STATUS	REASON
acme.com	web	-	needs attention	DNS points to another server
api.ada.ployz.test	api	-	needs attention	Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
```

### domain rm

```console
$ ployz domain rm api.ada.ployz.test
# exit 0
```

stdout:
```
Staged removal of domain api.ada.ployz.test on api in shop/production.
```

stderr:
```
next: ployz deploy
```

### deploy

```console
$ ployz deploy
# exit 124
```

stderr:
```
This deploy destroys 1 thing:
  ✕ removes domain api.ada.ployz.test
Approve 1cb9386e-37cb-4b10-813a-610393a31110 in Ployz Cloud at http://localhost:38265
next: ployz org approval approve 1cb9386e-37cb-4b10-813a-610393a31110
Waiting for approval in Ployz Cloud… 
Timed out after 300s
```

### domain add --json a generated domain

```console
$ ployz --json domain add api
# exit 0
```

stdout:
```
{
  "added": true,
  "domain": {
    "hostname": "api.ada.ployz.test",
    "kind": "generated",
    "port": null,
    "prefix": "api",
    "service": "api"
  },
  "environment": {
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 29
  },
  "free_prefix": null,
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
    "id": "e3ff9fdc-d394-4a02-9195-f717984132db",
    "name": "production",
    "project": "shop",
    "revision": 30
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
Discarded 1 staged change in shop/production: api.managedHostnames.
```

stderr:
```
inspect: ployz diff
```

## Remove a custom domain, then discard

### domain rm a custom domain

```console
$ ployz domain rm acme.com
# exit 0
```

stdout:
```
Staged removal of domain acme.com on web in shop/production.
```

stderr:
```
next: ployz deploy
```

### domain ls with a staged removal

```console
$ ployz domain ls
# exit 0
```

stdout:
```
DOMAIN	SERVICE	PORT	STATUS	REASON
api.ada.ployz.test	api	-	needs attention	Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)
```

### discard

```console
$ ployz discard
# exit 0
```

stdout:
```
Discarded 1 staged change in shop/production: web.routes.3e38d44d-322e-53af-97b6-6204849f6a5c.
```

stderr:
```
inspect: ployz diff
```

### domain add a custom domain that exists

```console
$ ployz domain add web acme.com
# exit 0
```

stdout:
```
Nothing changed in shop/production; acme.com is already on web.
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
```

