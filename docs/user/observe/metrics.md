---
title: Metrics
description: See how much CPU, memory, network and disk each service and server uses.
---

Ployz measures how much CPU, memory, network and disk each service and each server uses. Every
server measures itself every 10 seconds and keeps the numbers on its own disk, so you get them
without installing anything.

## See a service's metrics

```sh
# Metrics are CLI-only for now
ployz metrics web
```

Ployz prints the service's current values and a summary of the recent range. Add `--json` for
the full reply.

A service's numbers add up all of its replicas, on every server that runs one.

| Metric | What it counts |
| --- | --- |
| CPU | CPU time the service's replicas use |
| Memory | Memory in use, against the replica's limit when it has one. Cache the server can take back isn't counted. |
| Network | Bytes in and out, all of the replica's interfaces together. The split between your servers' private network and public traffic is a server metric. |
| Disk | Bytes the replicas read and write. On a server with ZFS, a volume's reads and writes come from ZFS and count once per service, however many replicas share it. |

## See a server's metrics

```sh
ployz metrics --server web-1
```

A server shows its CPU, memory and network, and how its disk space splits between volumes,
mirrors, logs and metrics, and everything else. A volume's usage against its storage limit
updates every minute.

## See older metrics

For the last 24 hours you get every 10 second reading. Ployz then keeps a 5 minute summary for
each service and deployment for 90 days, so longer ranges show fewer, broader points.

A replica that stopped or was replaced keeps its history under its service. So does a removed
service, until its 90 days run out.

## Good to know

- **Metrics stay on your servers.** Ployz doesn't copy them anywhere else, so a server you delete
  takes its metrics with it. To keep metrics longer, send them from your app to a metrics service.
- **Plain Docker volumes aren't measured**, and neither are files a service writes outside its
  volumes. Use a volume with a storage limit to see its usage. On a server that shows **Docker
  only**, Ployz warns when less than 10% of the disk is free.
- **Metrics give way when disk runs low.** The last day of readings shares the disk space Ployz
  keeps for logs. When that space fills, or the disk runs short of free space, the oldest hours
  go first. See [Logs](logs.md).
- **A missing number shows as not measured, never as zero.** A server that doesn't answer is
  named, and the other servers' numbers still show.
- **Metrics share a CPU budget with logs.** Ployz caps its helper for both at 10% of one CPU core.
  See [Logs](logs.md).
