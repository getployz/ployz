# Ployz

Ployz is a small-cluster orchestration core built around explicit operations
and a NATS-native control plane.

## Install `ployzctl`

```sh
curl -fsSL https://ployz.sh | sh
```

The default installer resolves the `alpha` channel to an exact GitHub release,
downloads the platform manifest, verifies SHA-256, and installs `ployzctl`.

Install an exact release when reproducibility matters:

```sh
curl -fsSL https://ployz.sh | sh -s -- --version v0.0.1-alpha.1
```

Select a channel explicitly:

```sh
curl -fsSL https://ployz.sh | sh -s -- --channel alpha
```

Machine bootstrap commands use the same installer, but keeper and substrate
updates require exact versions. Ployz does not use GitHub `latest`.
