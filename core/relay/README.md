# Ployz Relay

The self-hosted [iroh relay](https://github.com/n0-computer/iroh/tree/main/iroh-relay) that
the management transport uses when a Machine sits behind NAT. It runs as one Uncloud service
on the Hetzner host behind Caddy at `https://relay.ployz.dev`, the compiled default relay URL
in the core crate. Access is open; the relay's per-client rate limits in `iroh-relay.toml`
are the abuse control. Add an allowlist only if abuse appears.

The image is pinned to `n0computer/iroh-relay:v1.2.0`, matching the iroh 1.2 dependency of
the client and daemon crates. Bump both together.

## One-time setup

Create the dataset that backs the config volume:

```sh
ssh root@5.9.85.203 zfs create -o quota=10G data/ployz-relay
scp iroh-relay.toml root@5.9.85.203:/volumes/ployz-relay/iroh-relay.toml
```

Point `relay.ployz.dev` at the host. Caddy obtains the certificate on first request.

## Deploy

```sh
uc deploy -f compose.yaml
```

Config changes: copy `iroh-relay.toml` to `/volumes/ployz-relay/` again and redeploy.

Relay traffic is end-to-end encrypted between endpoints; the relay never sees RPC payloads.
QUIC address discovery is off because it needs the relay to terminate TLS itself on a UDP
port Caddy cannot front; enable it with a `[tls]` section if hole punching becomes necessary.
