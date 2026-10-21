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
web.ada.ployz.test	web	80	needs attention	Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)
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
    },
    {
      "action": null,
      "hostname": "web.ada.ployz.test",
      "kind": "generated",
      "port": 80,
      "prefix": "web",
      "reason": "Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)",
      "service": "web",
      "status": "needs_attention"
    }
  ],
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 40
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
# exit 0
```

stdout:
```
domain = web.ada.ployz.test
service = web
port = 80
status = needs attention
reason = Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)
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
    "reason": "Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)",
    "service": "web",
    "status": "needs_attention"
  },
  "environment": {
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 40
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
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 40
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
# exit 0
```

stdout:
```
Staged generated domain shopweb.ada.ployz.test on web in shop/production.
```

stderr:
```
next: ployz deploy
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
shopweb.ada.ployz.test	web	80	setting up	Live after your next deploy
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
next: ployz deploy
```

### diff

```console
$ ployz diff
# exit 0
```

stdout:
```
web (update)
  web.domains: removed web.ada.ployz.test
  web.domains: added shopweb.ada.ployz.test on port 80
```

stderr:
```
next: ployz deploy
```

### deploy (Ada approves removing web.ada.ployz.test)

```console
$ ployz deploy
# exit 0
```

stderr:
```
This deploy destroys 1 thing:
  ✕ removes domain web.ada.ployz.test
Approve 21c78985-bc45-4bae-a6a5-617855d65f9a in Ployz Cloud at http://localhost:33907
next: ployz org approval approve 21c78985-bc45-4bae-a6a5-617855d65f9a
Waiting for approval in Ployz Cloud… approved.
Deploying #32 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): booting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! shopweb.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
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
shopweb.ada.ployz.test	web	80	needs attention	Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)
```

stderr:
```
Add DNS for acme.com: AAAA @ 2001:db8::10
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
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 42
  },
  "next": "ployz deploy",
  "staged": [
    "web"
  ]
}
```

### deploy (Ada approves removing shopweb.ada.ployz.test)

```console
$ ployz deploy
# exit 0
```

stderr:
```
This deploy destroys 1 thing:
  ✕ removes domain shopweb.ada.ployz.test
Approve b6ff28c3-96ce-4441-b578-2ec60feaf429 in Ployz Cloud at http://localhost:33907
next: ployz org approval approve b6ff28c3-96ce-4441-b578-2ec60feaf429
Waiting for approval in Ployz Cloud… approved.
Deploying #33 of shop/production
  api (depth 0): unchanged
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): waiting 0/1
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: pending
! acme.com answers from another server. Point it at 2001:db8::10. A certificate cannot be issued until then.
! web.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  web (depth 1): booting 0/1
  web (depth 1): switching 1/1
  web (depth 1): switched 1/1
  Config sentry: unchanged
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
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
Deploying #34 of shop/production
  api (depth 0): waiting 0/1
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: unchanged
! api.ada.ployz.test is not published yet. Ployz Cloud publishes it once it reaches port 80 on 2001:db8::10.
  api (depth 0): booting 0/1
! api's image traefik/whoami:v1.11.0 exposes 80; its domains send to 8080. Set the domain port or PORT.
  api (depth 0): switching 1/1
  api (depth 0): switched 1/1
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
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
web.ada.ployz.test	web	80	needs attention	Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)
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

### deploy the removal (Ada approves it)

```console
$ ployz deploy
# exit 0
```

stderr:
```
This deploy destroys 1 thing:
  ✕ removes domain api.ada.ployz.test
Approve ec0852c6-b62f-4127-9be1-67644ab60840 in Ployz Cloud at http://localhost:33907
next: ployz org approval approve ec0852c6-b62f-4127-9be1-67644ab60840
Waiting for approval in Ployz Cloud… approved.
Deploying #35 of shop/production
  api (depth 0): waiting 0/1
  postgres (depth 0): unchanged
  redis (depth 0): unchanged
  web (depth 1): unchanged
  worker (depth 1): unchanged
  Volume pg-data: unchanged
  Config sentry: unchanged
  api (depth 0): booting 0/1
  api (depth 0): switching 1/1
  api (depth 0): switched 1/1
──────────────────────────────────────────
1 deployed · 6 unchanged · across 1 server
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
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 45
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
    "id": "2227decd-a593-411d-ba9e-62bb9ecfb404",
    "name": "production",
    "project": "shop",
    "revision": 46
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
Nothing staged in shop/production; nothing to discard.
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
web.ada.ployz.test	web	80	needs attention	Port 80 is closed on 2001:db8::10. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)
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

