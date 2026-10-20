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
ployz_<redacted>
```

stderr:
```
Made token ci (Full as you) in Organization ada, expiring 2027-01-18T09:50:28.004Z. Its secret is shown only now; set it as PLOYZ_TOKEN:
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
    "expires_at": "2026-10-27T09:50:28.335Z",
    "id": "1148f999-2026-4479-a2f8-0b2139304f02",
    "kind": "cli",
    "name": "deploy",
    "organization": "ada",
    "scope": "Full as you",
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
KIND	ID	NAME	SCOPE	EXPIRES	STATE
cli	6ba09df9-b90c-419d-84cf-db76edb8a0c0	verify	Full as you	2026-11-19T09:49:16.980Z	-
cli	7275ddd9-19c8-4f49-9cce-ef35a5d3f60f	verify	Full as you	2026-11-19T09:49:17.016Z	-
cli	87f85b1d-838d-41f7-87a7-dc970dd1424e	verify	Full as you	2026-11-19T09:49:17.034Z	-
cli	eaf5a551-4ef7-4b40-a6c4-b906216ee3da	ci	Full as you	2027-01-18T09:50:28.004Z	-
cli	1148f999-2026-4479-a2f8-0b2139304f02	deploy	Full as you	2026-10-27T09:50:28.335Z	-
device	6fde175d-6df2-4d2e-ae64-4b9fbffb1dec	ployz login 2026-10-20	Full as you	2026-10-27T09:50:26.117Z	current
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
      "created_at": "2026-10-20T09:50:26.117Z",
      "current": true,
      "expires_at": "2026-10-27T09:50:26.117Z",
      "id": "6fde175d-6df2-4d2e-ae64-4b9fbffb1dec"
    }
  ],
  "revoking": [],
  "tokens": [
    {
      "created_at": "2026-10-20T09:49:16.983Z",
      "current": false,
      "expired": false,
      "expires_at": "2026-11-19T09:49:16.980Z",
      "id": "6ba09df9-b90c-419d-84cf-db76edb8a0c0",
      "kind": "cli",
      "name": "verify",
      "scope": "Full as you"
    },
    {
      "created_at": "2026-10-20T09:49:17.017Z",
      "current": false,
      "expired": false,
      "expires_at": "2026-11-19T09:49:17.016Z",
      "id": "7275ddd9-19c8-4f49-9cce-ef35a5d3f60f",
      "kind": "cli",
      "name": "verify",
      "scope": "Full as you"
    },
    {
      "created_at": "2026-10-20T09:49:17.035Z",
      "current": false,
      "expired": false,
      "expires_at": "2026-11-19T09:49:17.034Z",
      "id": "87f85b1d-838d-41f7-87a7-dc970dd1424e",
      "kind": "cli",
      "name": "verify",
      "scope": "Full as you"
    },
    {
      "created_at": "2026-10-20T09:50:28.005Z",
      "current": false,
      "expired": false,
      "expires_at": "2027-01-18T09:50:28.004Z",
      "id": "eaf5a551-4ef7-4b40-a6c4-b906216ee3da",
      "kind": "cli",
      "name": "ci",
      "scope": "Full as you"
    },
    {
      "created_at": "2026-10-20T09:50:28.336Z",
      "current": false,
      "expired": false,
      "expires_at": "2026-10-27T09:50:28.335Z",
      "id": "1148f999-2026-4479-a2f8-0b2139304f02",
      "kind": "cli",
      "name": "deploy",
      "scope": "Full as you"
    }
  ]
}
```

### token rm ID

```console
$ ployz token rm eaf5a551-4ef7-4b40-a6c4-b906216ee3da
# exit 0
```

stdout:
```
Revoked token eaf5a551-4ef7-4b40-a6c4-b906216ee3da.
```

### token rm ID --json

```console
$ ployz token rm 1148f999-2026-4479-a2f8-0b2139304f02 --json
# exit 0
```

stdout:
```
{
  "removed": {
    "id": "1148f999-2026-4479-a2f8-0b2139304f02",
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
$ ployz token rm eaf5a551-4ef7-4b40-a6c4-b906216ee3da
# exit 1
```

stderr:
```
error: no token or signed-in device eaf5a551-4ef7-4b40-a6c4-b906216ee3da in this Organization
next: ployz token ls
```

### token rm, unknown ID

```console
$ ployz token rm 00000000-0000-0000-0000-000000000000
# exit 1
```

stderr:
```
error: no token or signed-in device 00000000-0000-0000-0000-000000000000 in this Organization
next: ployz token ls
```

### token rm, unknown ID --json

```console
$ ployz token rm 00000000-0000-0000-0000-000000000000 --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"next":"ployz token ls"},"message":"no token or signed-in device 00000000-0000-0000-0000-000000000000 in this Organization"}}
```

### token rm, not a UUID

```console
$ ployz token rm ci
# exit 1
```

stderr:
```
error: no token or signed-in device ci in this Organization
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
cloud = http://localhost:35249
organization = ada
signed in as = token
needs attention = Name a Project: this Organization has more than one
```

stderr:
```
next: ployz link --project PROJECT
```

### status with PLOYZ_TOKEN --json

```console
$ ployz status --json
# exit 0
```

stdout:
```
{
  "attention": [
    {
      "message": "Name a Project: this Organization has more than one",
      "next": "ployz link --project PROJECT",
      "reason": "ambiguous"
    }
  ],
  "deploying": [],
  "environment": null,
  "identity": {
    "cloud": "http://localhost:35249",
    "credential": "token",
    "organization": {
      "id": "5e293990-09ad-45c1-b636-6444de4fb848",
      "slug": "ada"
    }
  },
  "next": "ployz link --project PROJECT",
  "scope": {
    "environment": null,
    "link": null,
    "project": null
  },
  "staged": null
}
```

### token ls with PLOYZ_TOKEN

```console
$ ployz token ls
# exit 0
```

stdout:
```
KIND	ID	NAME	SCOPE	EXPIRES	STATE
cli	6ba09df9-b90c-419d-84cf-db76edb8a0c0	verify	Full as you	2026-11-19T09:49:16.980Z	-
cli	7275ddd9-19c8-4f49-9cce-ef35a5d3f60f	verify	Full as you	2026-11-19T09:49:17.016Z	-
cli	87f85b1d-838d-41f7-87a7-dc970dd1424e	verify	Full as you	2026-11-19T09:49:17.034Z	-
cli	3dc76578-e096-4e00-9b6b-12f30b6a70c8	env-test	Full as you	2027-01-18T09:50:31.503Z	current
device	6fde175d-6df2-4d2e-ae64-4b9fbffb1dec	ployz login 2026-10-20	Full as you	2026-10-27T09:50:26.117Z	-
```

### org ls with PLOYZ_TOKEN

```console
$ ployz org ls
# exit 0
```

stdout:
```
ORGANIZATION	NAME	CURRENT
ada	Ada Lovelace's Projects	current
```

### org use with PLOYZ_TOKEN (refused)

```console
$ ployz org use babbage
# exit 2
```

stderr:
```
error: PLOYZ_TOKEN acts only in the Organization it was made in
```

### token new with PLOYZ_TOKEN

```console
$ ployz token new nested
# exit 1
```

stderr:
```
error: Only a signed-in person can make tokens. No changes made.
next: unset PLOYZ_TOKEN && ployz login
```

### status with a revoked PLOYZ_TOKEN

```console
$ ployz status
# exit 1
```

stderr:
```
error: Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted
next: unset PLOYZ_TOKEN && ployz login
```

### token ls with a revoked PLOYZ_TOKEN --json

```console
$ ployz token ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"unset PLOYZ_TOKEN && ployz login"},"message":"Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted"}}
```

