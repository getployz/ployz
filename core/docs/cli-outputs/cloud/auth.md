# Ployz Cloud CLI outputs: auth

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## login from scratch, human (killed after 5s while it waits)

### login, waiting for approval (timed out by the capture)

```console
$ ployz login
# exit 124
```

stdout:
```
Open http://localhost:34733/device?user_code=L5XAY4VA and confirm the code L5XAY4VA.
Waiting for approval... 
```

### login --wait --json, waiting (timed out by the capture)

```console
$ ployz login --wait --json
# exit 124
```

stderr:
```
Open http://localhost:34733/device?user_code=L5XAY4VA and confirm the code L5XAY4VA.
Waiting for approval... 
```

## login: pending, then approved

### login --json (prints page and code at once)

```console
$ ployz login --json
# exit 0
```

stdout:
```
{
  "code": "B2M7BBGT",
  "expires_in": 1800,
  "next": "ployz login --wait",
  "status": "pending",
  "url": "http://localhost:34733/device?user_code=B2M7BBGT"
}
```

### login again while pending --json (resumes the same code)

```console
$ ployz login --json
# exit 0
```

stdout:
```
{
  "code": "B2M7BBGT",
  "expires_in": 1800,
  "next": "ployz login --wait",
  "status": "pending",
  "url": "http://localhost:34733/device?user_code=B2M7BBGT"
}
```

### login after approval (human, resumes the pending code)

```console
$ ployz login
# exit 0
```

stdout:
```
Open http://localhost:34733/device?user_code=B2M7BBGT and confirm the code B2M7BBGT.
Waiting for approval... Signed in to http://localhost:34733 as ada@example.com in Organization ada.
```

### login when already signed in

```console
$ ployz login
# exit 0
```

stdout:
```
Signed in to http://localhost:34733 as ada@example.com in Organization ada.
```

### login when already signed in --json

```console
$ ployz login --json
# exit 0
```

stdout:
```
{
  "account": {
    "email": "ada@example.com",
    "id": "9489ceb9-1172-4c9d-9a2f-cb3a2d69adce",
    "name": "Ada Lovelace"
  },
  "cloud": "http://localhost:34733",
  "organization": {
    "id": "a52ce0da-943d-4bfb-adfb-9da76e128533",
    "slug": "ada"
  },
  "status": "signed_in"
}
```

## status, signed in

### status

```console
$ ployz status
# exit 0
```

stdout:
```
Cloud: http://localhost:34733
Organization ada (ada@example.com).
Environment shop/production.
2 staged changes.
Deployment 1 is queued.
next: ployz diff
```

### status --json

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
    "account": {
      "email": "ada@example.com",
      "id": "9489ceb9-1172-4c9d-9a2f-cb3a2d69adce",
      "name": "Ada Lovelace"
    },
    "cloud": "http://localhost:34733",
    "credential": "device",
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

## logout

### logout

```console
$ ployz logout
# exit 0
```

stdout:
```
Signed out of http://localhost:34733.
```

### logout again

```console
$ ployz logout
# exit 0
```

stdout:
```
Not signed in.
```

### logout --json

```console
$ ployz logout --json
# exit 0
```

stdout:
```
{
  "cloud": "http://localhost:34733",
  "device": "86055a7c-dfa4-46f1-bcb2-b301f474130e",
  "servers": {
    "confirmed": [],
    "unconfirmed": []
  },
  "signed_out": true
}
```

