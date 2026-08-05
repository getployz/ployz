# Fast Dev Loop

Build one content-addressed local release bundle, then use that same directory
for bootstrap, machine add, and updates without publishing a GitHub release:

```sh
release="$(scripts/dev-release.sh build)"
ployz init root@core --local-release "$release"
ployz machine add root@edge --local-release "$release"
scripts/dev-push-substrate.sh --release "$release" root@core root@edge
```

The build uses the cached DinD artifact builder with incremental compilation.
The bundle contains the four verified Linux artifacts, installer, and manifest;
its version is derived from their contents. Hosts keep it under
`/var/lib/ployz/dev-releases/<version>` so bootstrap and join can use the same
verified files.

For an already-running cluster, `dev-push-substrate.sh` stages the bundle on
each host in parallel and runs:

```sh
/var/lib/ployz/dev-releases/<version>/ployz host substrate-update \
  --manifest-file /var/lib/ployz/dev-releases/<version>/release.env
```

Host Runner owns substrate install paths, verification, and restarts. The script
also replaces `/usr/local/bin/ployz` before running the staged update. Updating
existing hosts does not change the machine-join release template; use the same
bundle passed to `ployz init` when adding machines. Recovery is reinstall or
rejoin from the converged Corrosion rows.

Useful knobs:

```sh
PLOYZ_DEV_SKIP_BUILD=1 scripts/dev-release.sh build
PLOYZ_DIND_PLATFORM=linux/arm64 scripts/dev-release.sh build
scripts/dev-push-substrate.sh root@server-a # builds a bundle automatically
```

Use GitHub Releases only when validating public install/update behavior. For
runtime fixes, this loop is enough.
