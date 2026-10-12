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

Ployz prints one row per metric. **NOW** is the newest reading, and **AVG** and **PEAK** cover
the last hour. Add `--since` for another range, such as `--since 6h`, `--since 7d` or an RFC 3339
timestamp. Add `--json` for the full reply.

A service's numbers add up all of its replicas, on every server that runs one.

| Metric | What it counts |
| --- | --- |
| CPU | CPU time the service's replicas use, in vCPUs |
| Memory | Memory in use, against the replica's limit when it has one. Cache the server can take back isn't counted. |
| Net in, Net out | Bytes per second, all of the replica's interfaces together |
| Disk read, Disk write | Bytes per second the replicas read and write. A managed volume's reads and writes come from ZFS and count once per service, however many replicas share it. |

Below the table, Ployz counts the OOM kills, exits and restarts in the range. When the range holds no readings at all, as for a misspelled name or a service deployed seconds ago, Ployz prints **No readings** instead of a table.

## See a server's metrics

```sh
ployz metrics --server web-1
```

A server shows its CPU, memory, network per interface, and how its disk space splits between
volumes, mirrors, logs and metrics, and everything else. Managed volumes report their usage every
minute.

## See older metrics

For the last 24 hours you get every 10 second reading. After that Ployz keeps a 5 minute summary
for each service, deployment and server, so longer ranges show fewer, broader points. How far back the
summaries reach depends on the disk space Ployz keeps for logs and metrics. See
[Good to know](#good-to-know).

A replica that stopped or was replaced keeps its history under its service. So does a removed
service, until its summaries age out.

## Good to know

- **Metrics stay on your servers.** Ployz doesn't copy them anywhere else, so a server you delete
  takes its metrics with it. To keep metrics longer, send them from your app to a metrics service.
- **Plain Docker volumes aren't measured**, and neither are files a service writes outside its
  volumes. Use a managed volume to see its usage. On a server that shows **Docker only**, Ployz
  warns when less than 10% of the disk is free.
- **Metrics give way when disk runs low.** Readings and 5 minute summaries share the disk space
  Ployz keeps for logs. When that space fills, or the disk runs short of free space, the oldest
  hours go first. See [Logs](logs.md).
- **A missing number shows as not measured, never as zero.** A row reads **not measured** when no
  reading exists in the range, and **NOW** reads **-** when there's no recent reading. A server that doesn't answer is named, and the other servers' numbers still show.
- **Metrics share a CPU budget with logs.** Ployz caps its helper for both at 10% of one CPU core.
  See [Logs](logs.md).
