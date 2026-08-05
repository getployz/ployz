# CLI mesh access

After `ployz init root@<host>` succeeds, the CLI keeps the operator peer's
WireGuard identity and the cluster's public dial context under
`~/.ployz/mesh/`. Later commands, including `ployz machine ls`, use that
persisted peer identity to reach the HTTP API over the cluster mesh. Reusing
the same context does not enroll another peer.

Use `--target` to select the context created through a particular founding
target:

```console
ployz machine ls --target root@192.0.2.10
```

The builtin provider uses an unprivileged userspace WireGuard connection. It
does not create a kernel interface and does not require root. Keep UDP port
51820 reachable from the operator machine to at least one cluster machine.

## UDP-blocked networks

The CLI reports a bounded failure when the WireGuard handshake cannot complete.
It never silently changes transport and never automatically opens an SSH
tunnel. On a network that blocks UDP, open a local forward explicitly in a
separate terminal:

```console
ssh -N -L 127.0.0.1:2020:[<machine-api-v6>]:2020 <founding-target>
```

Read the canonical machines lens through the forward:

```console
curl --fail --silent --show-error http://127.0.0.1:2020/lenses/machines
```

The SSH server opens the API connection on the remote machine, so this request
is re-originated and authorized as that machine's `Principal::Machine`, not as
the laptop's persisted `Principal::Peer`. The forward is therefore a manual
read and diagnostic fallback, not part of the normal `ployz machine ls` dial.
The CLI never opens it automatically. Stop the `ssh` process to close it.
