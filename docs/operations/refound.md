# Refound a cluster

Refound is the cluster-scope escape hatch for a destructive Corrosion schema
change, a Corrosion upgrade that cannot be performed in place, catastrophic
store repair, compaction, or a cluster whose durable rows cannot be trusted.
It is a runbook, not a `ployz` command: Ployz has no re-init, reseed, or
identity-recovery primitive.

Refounding creates a new cluster with a new `cluster_id`, WireGuard address
space, and machine identities. Save the namespace and deploy input you need
before beginning; the new cluster does not inherit prior intent.

## Before starting

- Choose one host to become the new machine one and make sure you can run its
  on-host commands with `sudo`.
- Keep the operator machine available to run remote CLI commands and retain
  the deploy input required to declare namespaces and deploy services again.
- Expect public ingress and internal DNS to be unavailable while gateway and
  DNS roles are stopped. Docker containers, Docker volumes, Ployz-provisioned
  volumes, and workload images remain in place: `machine reset` never stops
  Docker or removes workload storage.

## Refound

1. On every old cluster host, run the local reset primitive:

   ```console
   sudo ployz machine reset
   ```

   This stops Ployz and Corrosion and removes the local control-plane state,
   while preserving workload-volume storage. Existing workload containers
   continue to run, but Ployz no longer supplies their gateway, DNS, or
   certificate machinery.

2. Found the replacement cluster from the operator machine:

   ```console
   ployz init root@<machine-one-ip>
   ```

3. Mint a fresh join token, then paste its generated join line on each
   remaining host:

   ```console
   ployz token create bootstrap
   # on each host:
   sudo ployz machine join pzjoin_...
   ```

   Joining is the only way a reset host enters the replacement cluster. Each
   join creates a new machine identity.

4. Re-create namespaces and deploy every service from the saved input. Fresh
   ACME issuance restores TLS for routes that use it.

## Repair one contaminated or wiped machine

Do not use the refound steps for one machine. Compose the repair kit instead.

For a host that is still a member but contains a foreign identity, fence it
from the operator machine before changing the host:

```console
ployz machine rm <machine-name>
# on that host:
sudo ployz machine reset
# from the operator machine:
ployz token create bootstrap
# on that host, using the fresh token:
sudo ployz machine join pzjoin_...
```

For a wiped disk, the old identity and WireGuard key are gone. Remove the
corpse roster row first, then create a token and join the repaired host as a
new machine. The removal must come first because the old row would otherwise
win a same-name claim.

`machine rm` commits its roster deletion and testimony sweep as one
identity-scoped Corrosion write. If the client loses the reply, retry the same
remote removal with the returned or recorded machine id:

```console
ployz machine rm <machine-name>
```

Never substitute a targeted testimony-row delete, SSH-driven reset,
`machine rejoin`, `ployz init` on the existing cluster, `reseed`, or a force
flag. Those paths either leave the writer able to recreate bad rows or claim
to resurrect an identity that no longer exists.
