---
title: Metrics
description: See how much CPU, memory, network and disk each service and server uses.
---

Ployz measures how much CPU, memory, network and disk each service and each server uses. Every
server measures itself every 10 seconds and keeps the numbers on its own disk, so you get them
without installing anything.

## See a service's metrics

1. Click the service on the canvas.
2. Open its **Monitoring** tab.

A service with a public address leads with its [uptime](uptime.md) and **Response time**. Then
come **CPU**, **Memory**, **Network** and **Disk**, all on one time axis, so hovering one chart
moves the cursor on every chart. **CPU** reads as a share of one core, so 100% is one full
core. A dashed line on **CPU** and **Memory** marks the service's limit. A vertical rule marks
each deploy, and a marker shows each OOM kill, exit and restart. A service with a managed volume ends with a **Managed volume** row that names the volume and its
size limit.

The charts cover the last hour. Pick a range from 5 minutes to 30 days with the same picker as
uptime. On a service that runs on more than one server, **By server** draws one line per server
in place of the total.

<!-- screenshot: the Monitoring tab of web, with CPU, Memory, Network and Disk -->

From the CLI:

```sh
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

## Spot trouble on the canvas

When a service's replicas were OOM killed in the last hour, its card shows **⚠ OOM**. When one
replica crashed with a non-zero exit code and came back twice, each time within 10 minutes of the
last, the card shows **⚠ Restarting** until that replica stays up for 10 minutes. A restart after a clean
exit or a stop, such as `ployz service restart`, a Deploy or a rollback, shows nothing, even when
the app exits non-zero on the stop signal, as with SIGTERM (143), SIGINT (130) or SIGKILL (137).
When other issues share the card, the badge adds their count, as in **⚠ OOM +1**. The badge is
amber, or red when the service is down. Click the ⚠ to open the service's Monitoring tab. A
server that needs an upgrade or doesn't answer adds nothing to the card.

A crash or OOM kill also emails the organization, as
[Notifications](../account/notifications.md) describes.

## See a server's metrics

```sh
ployz metrics --server web-1
```

A server shows its CPU, memory, network per interface, and how its disk space splits between
volumes, mirrors, logs and metrics, and everything else. Managed volumes report their usage every
minute.

## Ask an agent

The [Ployz agent](../account/agent-sidebar.md) and `ployz mcp` read the same numbers through the
`service_metrics` and `server_metrics` tools, with each OOM kill, exit and restart in the range
and the server it happened on. Give `service_metrics` a deployment number and the range starts
when that deployment did, so the agent can tell an OOM kill from an app that exited with an
error. Both tools need Ployz Cloud. When a deployment failed, `deployment_show` names
`service_logs` and `service_metrics` for the failed service and that deployment, so the agent
reads both without picking a range.

## See older metrics

For the last 23 hours you get every 10 second reading. After that Ployz keeps a 5 minute summary
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
- **A missing number shows as not measured, never as zero.** A chart hatches the stretch no
  server measured as **Not measured**, and a chart with nothing in the range reads **No
  readings**. In the CLI, a row reads **not measured** when no reading exists in the range, and
  **NOW** reads **-** when there's no recent reading.
- **A server that doesn't answer is named.** The other servers' numbers still show, under a note
  such as "web-2 didn't answer."
- **A server on an older Ployz has no metrics to show.** The **Monitoring** tab names it, as in
  "web-2 needs an upgrade to show its metrics." When every server running the service needs one,
  the charts give way to **Upgrade web-2 to see its metrics** with a link to the server. See
  [Upgrade Ployz on a server](../servers/manage-servers.md#upgrade-ployz-on-a-server).
- **Metrics share a CPU budget with logs.** Ployz caps its helper for both at 10% of one CPU core.
  See [Logs](logs.md).
