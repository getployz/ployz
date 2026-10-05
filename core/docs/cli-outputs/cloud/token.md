# Ployz Cloud CLI outputs: token

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## token new / ls / rm

### token new ci

```console
$ ployz token new ci
# exit 0
```

stdout:
```
Made token ci (a261eda1-8592-433b-b55f-21aaad1f75cf) in Organization ada, expiring 2027-01-03T01:12:26.123Z.
Its secret is shown only now; set it as PLOYZ_TOKEN:
ployz_<redacted>
```

### token new deploy --expires-in 7 --json

```console
$ ployz token new deploy --expires-in 7 --json
# exit 0
```

stdout:
```
{
  "token": {
    "expires_at": "2026-10-12T01:12:26.241Z",
    "id": "3b320fee-0a08-47d8-b8d9-a6b9eb4a4d31",
    "name": "deploy",
    "organization": "ada",
    "secret": "ployz_<redacted>"
  }
}
```

### token new bad --expires-in

```console
$ ployz token new x --expires-in soon
# exit 2
```

stderr:
```
error: invalid value 'soon' for '--expires-in <DAYS>': invalid digit found in string

For more information, try '--help'.
```

### token ls

```console
$ ployz token ls
# exit 0
```

stdout:
```
KIND	ID	NAME	EXPIRES
token	a261eda1-8592-433b-b55f-21aaad1f75cf	ci	2027-01-03T01:12:26.123Z
token	3b320fee-0a08-47d8-b8d9-a6b9eb4a4d31	deploy	2026-10-12T01:12:26.241Z
device	54b92009-ff80-402e-a3a0-25cf1b1c702d	- *	2026-10-12T01:12:24.965Z
```

### token ls --json

```console
$ ployz token ls --json
# exit 0
```

stdout:
```
{
  "devices": [
    {
      "created_at": "2026-10-05T01:12:24.965Z",
      "current": true,
      "expires_at": "2026-10-12T01:12:24.965Z",
      "id": "54b92009-ff80-402e-a3a0-25cf1b1c702d"
    }
  ],
  "revoking": [],
  "tokens": [
    {
      "created_at": "2026-10-05T01:12:26.124Z",
      "current": false,
      "expired": false,
      "expires_at": "2027-01-03T01:12:26.123Z",
      "id": "a261eda1-8592-433b-b55f-21aaad1f75cf",
      "name": "ci"
    },
    {
      "created_at": "2026-10-05T01:12:26.242Z",
      "current": false,
      "expired": false,
      "expires_at": "2026-10-12T01:12:26.241Z",
      "id": "3b320fee-0a08-47d8-b8d9-a6b9eb4a4d31",
      "name": "deploy"
    }
  ]
}
```

### token rm ID

```console
$ ployz token rm a261eda1-8592-433b-b55f-21aaad1f75cf
# exit 0
```

stdout:
```
Revoked token a261eda1-8592-433b-b55f-21aaad1f75cf.
```

### token rm ID --json

```console
$ ployz token rm 3b320fee-0a08-47d8-b8d9-a6b9eb4a4d31 --json
# exit 0
```

stdout:
```
{
  "removed": {
    "id": "3b320fee-0a08-47d8-b8d9-a6b9eb4a4d31",
    "kind": "token"
  },
  "servers": {
    "confirmed": [],
    "unconfirmed": []
  }
}
```

### token rm, already revoked ID

```console
$ ployz token rm a261eda1-8592-433b-b55f-21aaad1f75cf
# exit 1
```

stderr:
```
no token or signed-in device a261eda1-8592-433b-b55f-21aaad1f75cf in this Organization
next: ployz token ls
```

### token rm, unknown ID

```console
$ ployz token rm 00000000-0000-0000-0000-000000000000
# exit 1
```

stderr:
```
no token or signed-in device 00000000-0000-0000-0000-000000000000 in this Organization
next: ployz token ls
```

### token rm, unknown ID --json

```console
$ ployz token rm 00000000-0000-0000-0000-000000000000 --json
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"next":"ployz token ls"},"message":"no token or signed-in device 00000000-0000-0000-0000-000000000000 in this Organization"}}
```

### token rm, not a UUID

```console
$ ployz token rm ci
# exit 1
```

stderr:
```
no token or signed-in device ci in this Organization
next: ployz token ls
```

## acting with PLOYZ_TOKEN

### status with PLOYZ_TOKEN

```console
$ ployz status
# exit 0
```

stdout:
```
Cloud: http://localhost:34733
Organization ada (token).
Environment shop/production.
2 staged changes.
Deployment 1 is queued.
next: ployz diff
```

### status with PLOYZ_TOKEN --json

```console
$ ployz status --json
# exit 0
```

stdout:
```
{
  "attention": [],
  "deploying": [
    {
      "admitted_at": 1791162709,
      "admitted_by": "Ada Lovelace",
      "ended_at": null,
      "environment_id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
      "id": "939f4f20-8e4a-40a5-a812-52c2c614d9f6",
      "in_flight": true,
      "message": "Ship everything",
      "number": 1,
      "outcome": null,
      "remove": false,
      "runner": null,
      "saved": 1,
      "services": [],
      "started_at": null,
      "status": "queued",
      "upload": null
    }
  ],
  "environment": {
    "id": "9d524a17-4056-47c0-9fc8-c7329841a90b",
    "name": "production",
    "project": "shop",
    "revision": 9
  },
  "identity": {
    "cloud": "http://localhost:34733",
    "credential": "token",
    "organization": {
      "id": "a52ce0da-943d-4bfb-adfb-9da76e128533",
      "slug": "ada"
    }
  },
  "next": "ployz diff",
  "scope": {
    "environment": null,
    "link": null,
    "project": null
  },
  "staged": {
    "changes": 2,
    "published": false,
    "version": "9:1:1.0"
  }
}
```

### token ls with PLOYZ_TOKEN

```console
$ ployz token ls
# exit 0
```

stdout:
```
KIND	ID	NAME	EXPIRES
token	3d4d8a57-5de6-4a3a-b01e-eb0083a224f1	env-test *	2027-01-03T01:12:27.992Z
device	54b92009-ff80-402e-a3a0-25cf1b1c702d	-	2026-10-12T01:12:24.965Z
```

### org ls with PLOYZ_TOKEN

```console
$ ployz org ls
# exit 0
```

stdout:
```
ORGANIZATION	NAME
ada *	Ada Lovelace's Projects
```

### org use with PLOYZ_TOKEN (refused)

```console
$ ployz org use babbage
# exit 1
```

stderr:
```
PLOYZ_TOKEN acts only in the Organization it was made in
```

### token new with PLOYZ_TOKEN

```console
$ ployz token new nested
# exit 0
```

stdout:
```
Made token nested (ce5f9a6e-43f9-4817-9189-ec59fae88396) in Organization ada, expiring 2027-01-03T01:12:29.603Z.
Its secret is shown only now; set it as PLOYZ_TOKEN:
ployz_<redacted>
```

### status with a revoked PLOYZ_TOKEN

```console
$ ployz status
# exit 1
```

stderr:
```
Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, or its maker left its Organization
next: ployz token new
```

### token ls with a revoked PLOYZ_TOKEN --json

```console
$ ployz token ls --json
# exit 1
```

stdout:
```
{"error":{"code":"unauthenticated","details":{"next":"ployz token new"},"message":"Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, or its maker left its Organization"}}
```

