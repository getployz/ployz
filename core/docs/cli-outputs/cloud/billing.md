# Ployz Cloud CLI outputs: billing

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## Billing off (self-hosted verify Cloud)

### billing (billing off)

```console
$ ployz billing
# exit 0
```

stdout:
```
Organization ada: self-hosted, no billing; custom domains allowed.
```

### billing --json (billing off)

```console
$ ployz billing --json
# exit 0
```

stdout:
```
{
  "billing": {
    "custom_domains": true,
    "organization": "ada",
    "plan": "self_hosted"
  }
}
```

### billing upgrade (billing off)

```console
$ ployz billing upgrade
# exit 1
```

stderr:
```
Cloud at http://localhost:34733 has no billing: it is self-hosted
```

### billing upgrade --json (billing off)

```console
$ ployz billing upgrade --json
# exit 1
```

stdout:
```
{"error":{"code":"unsupported","details":null,"message":"Cloud at http://localhost:34733 has no billing: it is self-hosted"}}
```

### billing manage (billing off)

```console
$ ployz billing manage
# exit 1
```

stderr:
```
Cloud at http://localhost:34733 has no billing: it is self-hosted
```

### billing manage --json (billing off)

```console
$ ployz billing manage --json
# exit 1
```

stdout:
```
{"error":{"code":"unsupported","details":null,"message":"Cloud at http://localhost:34733 has no billing: it is self-hosted"}}
```

## Billing on: not captured (BILLING=1 verify seed fails: custom domain on web needs Ployz Pro)

