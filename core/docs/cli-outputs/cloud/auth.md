# Ployz Cloud CLI outputs: auth

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## login from scratch, human (killed after 5s while it waits)

### login, waiting for approval (timed out by the capture)

```console
$ ployz login
# exit 124
```

stderr:
```
Open http://localhost:35249/device?user_code=49WME95H and confirm the code 49WME95H.
Waiting for approval... 
```

### login --wait --json, waiting (timed out by the capture)

```console
$ ployz login --wait --json
# exit 124
```

stderr:
```
Open http://localhost:35249/device?user_code=49WME95H and confirm the code 49WME95H.
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
  "code": "PAX8GKJ8",
  "expires_in": 1800,
  "next": "ployz login --wait",
  "status": "pending",
  "url": "http://localhost:35249/device?user_code=PAX8GKJ8"
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
  "code": "PAX8GKJ8",
  "expires_in": 1800,
  "next": "ployz login --wait",
  "status": "pending",
  "url": "http://localhost:35249/device?user_code=PAX8GKJ8"
}
```

### login after approval (human, resumes the pending code)

```console
$ ployz login
# exit 0
```

stdout:
```
Signed in to http://localhost:35249 as ada@example.com in Organization ada.
```

stderr:
```
Open http://localhost:35249/device?user_code=PAX8GKJ8 and confirm the code PAX8GKJ8.
Waiting for approval... 
```

### login when already signed in

```console
$ ployz login
# exit 0
```

stdout:
```
Signed in to http://localhost:35249 as ada@example.com in Organization ada.
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
    "id": "63a8db86-9c2e-4a79-9f74-3886d1245c48",
    "name": "Ada Lovelace"
  },
  "cloud": "http://localhost:35249",
  "organization": {
    "id": "5e293990-09ad-45c1-b636-6444de4fb848",
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
cloud = http://localhost:35249
organization = ada
signed in as = ada@example.com
needs attention = Name a Project: this Organization has more than one
```

stderr:
```
next: ployz link --project PROJECT
```

### status --json

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
    "account": {
      "email": "ada@example.com",
      "id": "63a8db86-9c2e-4a79-9f74-3886d1245c48",
      "name": "Ada Lovelace"
    },
    "cloud": "http://localhost:35249",
    "credential": "device",
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

## logout

### logout

```console
$ ployz logout
# exit 0
```

stdout:
```
Signed out of http://localhost:35249.
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
  "cloud": "http://localhost:35249",
  "device": "38b91fb7-ae79-42b6-8c95-f3b89c8e292a",
  "servers": {
    "confirmed": [],
    "unconfirmed": []
  },
  "signed_out": true
}
```

