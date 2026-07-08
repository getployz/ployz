# Fast Dev Loop

Use this for hcloud validation when GitHub Releases and the alpha channel are
too slow. It builds local Linux artifacts and installs them straight onto test
servers.

```sh
scripts/dev-push-substrate.sh root@server-a root@server-b root@server-c root@server-d
```

By default the script builds `linux/amd64` artifacts with the existing Docker
builder once, copies them to temporary directories on each server in parallel,
writes a local release manifest, and runs the staged keeper:

```sh
/tmp/ployz-dev-push-*/ployz-keeper substrate-update --manifest-file /tmp/ployz-dev-push-*/release.env
```

Keeper owns substrate install paths, verification, and restarts. The script
also replaces `/usr/local/bin/ployz-keeper` before running the staged update,
and does not touch NATS or release channel state.

Before a hub-loss promotion test, run this on the intended promotion candidate
after it has been connected long enough to receive an intent drumbeat:

```sh
ployz-keeper core-promote --check
```

It fails fast if `core-seeds.key`, `ca-recovery.key`, or
`intent-mirror.json` is missing or the mirror cannot be parsed.

Useful knobs:

```sh
PLOYZ_DEV_SKIP_BUILD=1 scripts/dev-push-substrate.sh root@server-a
PLOYZ_DIND_PLATFORM=linux/arm64 scripts/dev-push-substrate.sh root@arm-server
```

Use GitHub Releases only when validating public install/update behavior. For
runtime fixes, this loop is enough.
