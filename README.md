# Ployz

Ployz is a small-cluster orchestration core built around explicit operations
and a NATS-native control plane.

## Bootstrap A Machine

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap
```

The default installer resolves the `alpha` channel to an exact GitHub release,
downloads the platform manifest, verifies SHA-256, and installs only
`ployz-keeper`.

Install an exact release when reproducibility matters:

```sh
curl -fsSL https://ployz.sh | sh -s -- --version v0.0.1-alpha.1 && sudo ployz-keeper bootstrap
```

Select a channel explicitly:

```sh
curl -fsSL https://ployz.sh | sh -s -- --channel alpha && sudo ployz-keeper bootstrap
```

For automation or cloud-init, pass the Cloud token to keeper after installation:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap --cloud-token pcbs_...
```

`ployz.sh` is Bootstrap Delivery only. It does not accept Cloud tokens, install
`ployzctl`, choose a Cloud org, or decide founder vs joiner bootstrap.
The public bootstrap installer targets Linux machines.

The workstation-driven local path remains:

```sh
ployzctl machine init USER@HOST
```

TODO: When the `ployzctl` Homebrew tap exists, recommend it here for
workstation CLI installs.

Ployz does not use GitHub `latest`.

Release publishing and channel promotion are documented in
[`docs/operations/release.md`](docs/operations/release.md).
