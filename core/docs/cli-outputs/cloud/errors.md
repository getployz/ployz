# Ployz Cloud CLI outputs: errors

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## Not signed in (fresh HOME, PLOYZ_CLOUD_URL set)

### not signed in: status

```console
$ ployz status
# exit 0
```

stdout:
```
signed in as = no one
```

stderr:
```
next: ployz login
```

### not signed in: status --json

```console
$ ployz status --json
# exit 0
```

stdout:
```
{
  "attention": [],
  "deploying": [],
  "environment": null,
  "identity": null,
  "next": "ployz login",
  "scope": {
    "environment": null,
    "link": null,
    "project": null
  },
  "staged": null
}
```

### not signed in: token ls

```console
$ ployz token ls
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: token ls --json

```console
$ ployz token ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: org ls

```console
$ ployz org ls
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: org ls --json

```console
$ ployz org ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: github ls

```console
$ ployz github ls
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: github ls --json

```console
$ ployz github ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: domain ls

```console
$ ployz domain ls
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: domain ls --json

```console
$ ployz domain ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: deployment ls

```console
$ ployz deployment ls
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: deployment ls --json

```console
$ ployz deployment ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: deploy --plan

```console
$ ployz deploy --plan
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: deploy --plan --json

```console
$ ployz deploy --plan --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: logs

```console
$ ployz logs
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: logs --json

```console
$ ployz logs --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: ps

```console
$ ployz ps
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: ps --json

```console
$ ployz ps --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### not signed in: cloud reset -y

```console
$ ployz cloud reset -y
# exit 1
```

stderr:
```
error: Not signed in to Cloud.
next: ployz login
```

### not signed in: cloud reset -y --json

```console
$ ployz cloud reset -y --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"ployz login"},"message":"Not signed in to Cloud."}}
```

### logout when not signed in

```console
$ ployz logout
# exit 0
```

stdout:
```
Not signed in.
```

### logout when not signed in --json

```console
$ ployz logout --json
# exit 0
```

stdout:
```
{
  "cloud": null,
  "device": null,
  "servers": null,
  "signed_out": false
}
```

## Unreachable Cloud

### login, Cloud unreachable

```console
$ ployz login
# exit 1
```

stderr:
```
error: Could not reach Cloud at http://127.0.0.1:9.
  cause: Connection refused (os error 111)
```

### login, Cloud unreachable --json

```console
$ ployz login --json
# exit 1
```

stdout:
```
{"error":{"cause":["error sending request","client error (Connect)","tcp connect error","Connection refused (os error 111)"],"code":"unavailable","details":null,"message":"Could not reach Cloud at http://127.0.0.1:9."}}
```

### token ls, token against unreachable Cloud

```console
$ ployz token ls
# exit 1
```

stderr:
```
error: Could not reach Cloud at http://127.0.0.1:9.
  cause: Connection refused (os error 111)
```

### token ls, token against unreachable Cloud --json

```console
$ ployz token ls --json
# exit 1
```

stdout:
```
{"error":{"cause":["error sending request","client error (Connect)","tcp connect error","Connection refused (os error 111)"],"code":"unavailable","details":null,"message":"Could not reach Cloud at http://127.0.0.1:9."}}
```

### login --json, URL that is not a Ployz Cloud

```console
$ ployz login --json
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"Cloud answered HTTP 405"}}
```

## Bad PLOYZ_TOKEN

### bogus PLOYZ_TOKEN: status

```console
$ ployz status
# exit 1
```

stderr:
```
error: Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted
next: unset PLOYZ_TOKEN && ployz login
```

### bogus PLOYZ_TOKEN: status --json

```console
$ ployz status --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"unset PLOYZ_TOKEN && ployz login"},"message":"Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted"}}
```

### bogus PLOYZ_TOKEN: token ls

```console
$ ployz token ls
# exit 1
```

stderr:
```
error: Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted
next: unset PLOYZ_TOKEN && ployz login
```

### bogus PLOYZ_TOKEN: token ls --json

```console
$ ployz token ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"unset PLOYZ_TOKEN && ployz login"},"message":"Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted"}}
```

### bogus PLOYZ_TOKEN: org ls

```console
$ ployz org ls
# exit 1
```

stderr:
```
error: Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted
next: unset PLOYZ_TOKEN && ployz login
```

### bogus PLOYZ_TOKEN: org ls --json

```console
$ ployz org ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"unset PLOYZ_TOKEN && ployz login"},"message":"Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted"}}
```

### bogus PLOYZ_TOKEN: deployment ls

```console
$ ployz deployment ls
# exit 1
```

stderr:
```
error: Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted
next: unset PLOYZ_TOKEN && ployz login
```

### bogus PLOYZ_TOKEN: deployment ls --json

```console
$ ployz deployment ls --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"unauthenticated","details":{"next":"unset PLOYZ_TOKEN && ployz login"},"message":"Cloud refused PLOYZ_TOKEN: it is unknown, revoked or expired, its maker left its Organization, or its Organization was deleted"}}
```

## Usage errors (clap)

### token new without a name

```console
$ ployz token new
# exit 2
```

stderr:
```
error: the following required arguments were not provided:
  <name>

Usage: ployz token new <name>

For more information, try '--help'.
```

### token new without a name --json

```console
$ ployz token new --json
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"the following required arguments were not provided:\n  <name>\n\nUsage: ployz token new --json <name>\n\nFor more information, try '--help'."}}
```

### org use without a slug

```console
$ ployz org use
# exit 2
```

stderr:
```
error: the following required arguments were not provided:
  <organization>

Usage: ployz org use <organization>

For more information, try '--help'.
```

### unknown subcommand

```console
$ ployz org frobnicate
# exit 2
```

stderr:
```
error: unrecognized subcommand 'frobnicate'

Usage: ployz org [OPTIONS] [COMMAND]

For more information, try '--help'.
```

### bad build-order value

```console
$ ployz org build-order sideways
# exit 2
```

stderr:
```
error: invalid value 'sideways' for '[order]'
  [possible values: auto, servers-only, github-then-servers, servers-then-github, github-only]

For more information, try '--help'.
```

### domain add without a service

```console
$ ployz domain add
# exit 2
```

stderr:
```
error: the following required arguments were not provided:
  <service>

Usage: ployz domain add <service> [host]

For more information, try '--help'.
```

