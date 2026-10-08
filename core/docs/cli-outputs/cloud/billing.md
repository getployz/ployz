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
Organization ada: self-hosted, no billing.
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
    "organization": "ada",
    "plan": "self_hosted"
  }
}
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
