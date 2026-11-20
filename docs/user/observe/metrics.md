---
title: Metrics
description: See how much CPU, memory, network and disk each service and server uses.
---

Ployz measures how much CPU, memory, network and disk each service and each server uses. Every
server reads its own numbers every 10 seconds and sends them to Ployz Cloud, which draws the
charts and keeps the history for 30 days. Nothing to install on your services.

Metrics need Ployz Cloud with the `observe` profile on. Without it, the charts say **Metrics
aren't enabled on this Service**, and `ployz metrics` shows live numbers only. See
[Keep logs, metrics and traces in Cloud](../self-hosting.md#keep-logs-metrics-and-traces-in-cloud-optional).

## See a service's metrics

1. Click the service on the canvas.
2. Open its **Monitoring** tab.

A service with a public address leads with its [uptime](uptime.md) and **Response time** when the
Organization has [Production](../account/organizations.md#billing). Then come **CPU**, **Memory**, **Network** and **Disk**, all on one time axis, so hovering one chart
moves the cursor on every chart. **CPU** reads as a share of one core, so 100% is one full
core. A dashed line on **CPU** and **Memory** marks the service's limit. A vertical rule marks
each deploy, and a marker shows each OOM kill and exit. A service with a managed volume ends with a **Managed volume** row that names the volume and its
size limit.

The charts cover the last hour. Pick a range from 5 minutes to 30 days with the same picker as
uptime. On a service that runs on more than one server, **By server** draws one line per server
in place of the total.

<!-- screenshot: the Monitoring tab of web, with CPU, Memory, Network and Disk -->

A service's numbers add up all of its replicas, on every server that runs one.

| Metric | What it counts |
| --- | --- |
| CPU | CPU time the service's replicas use, in vCPUs |
| Memory | Memory in use, against the replica's limit when it has one. Cache the server can take back isn't counted. |
| Net in, Net out | Bytes per second, all of the replica's interfaces together |
| Disk read, Disk write | Bytes per second the replicas read and write. A managed volume's reads and writes come from ZFS and count once per service, however many replicas share it. |

A counter that starts over, as when a replica restarts, never draws a dip below zero.

## Read metrics from the CLI

```sh
ployz metrics web
```

Without Cloud, `ployz metrics` reads Docker on your servers and prints the numbers right now:
how many containers the service runs, its CPU and memory, and the bytes its containers have read,
written, received and sent in total. It ends with **History is in Ployz Cloud.**

With a Cloud credential, `ployz metrics` prints one row per metric from Cloud's history. **NOW** is
the newest reading, and **AVG** and **PEAK** cover the last hour. Add `--since` for another
range, such as `--since 6h`, `--since 7d`, `--since 2026-01-31` or an RFC 3339 timestamp, and
`--json` for the full reply. Below the table, Ployz counts the OOM kills and exits in the range.
`--since` has no effect without Cloud, which keeps no history. On a Cloud without metrics
enabled, `ployz metrics` prints live numbers and says **Metrics history isn’t enabled on this
Cloud.**

A name the environment doesn't have, and that has no readings, fails with
`No Service named nosvc in Environment production` and lists the services you can name. It exits
with code 1.

## Spot trouble on the canvas

When a service's replicas were OOM killed in the last hour, its card shows **⚠ OOM**. When one
replica crashed with a non-zero exit code twice, each time within 10 minutes of the last, the
card shows **⚠ Restarting** until that replica stays up for 10 minutes. An exit after a stop, such
as `ployz service restart`, a Deploy or a rollback, shows nothing, even when
the app exits non-zero on the stop signal, as with SIGTERM (143), SIGINT (130) or SIGKILL (137).
When other issues share the card, the badge adds their count, as in **⚠ OOM +1**. The badge is
amber, or red when the service is down. Click the ⚠ to open the service's Monitoring tab. A
server that doesn't answer adds nothing to the card. With Production, a failing
uptime check shows **⚠ Unreachable**, and a failing healthcheck **⚠ Check failing**.

A crash or OOM kill also emails the organization, as
[Notifications](../account/notifications.md) describes.

## See a server's metrics

Open a server from **Servers** to see its CPU, memory, network per interface, and how its disk
space splits between volumes, mirrors, logs and metrics, and everything else. Managed volumes
report their usage every minute.

```sh
ployz metrics --server web-1
```

Without Cloud, this prints the server's live numbers only.

## Ask an agent

The [Ployz agent](../account/agent-sidebar.md) and `ployz mcp` read the same numbers through the
`service_metrics` and `server_metrics` tools, with each OOM kill and exit in the range and the
server it happened on. Give `service_metrics` a deployment number and the range starts when that
deployment did, so the agent can tell an OOM kill from an app that exited with an error. Both
tools need Ployz Cloud. When a deployment failed, `deployment_show` names
`service_logs` and `service_metrics` for the failed service and that deployment, so the agent
reads both without picking a range.

## When a chart is empty

Every metrics view says why it has nothing to show:

- **Metrics aren't enabled on this Service** (or **Server**). Cloud has no ClickHouse. Start the
  `observe` profile and set `OBSERVE_CLICKHOUSE_URL`, as
  [self-hosting](../self-hosting.md#keep-logs-metrics-and-traces-in-cloud-optional) describes.
- **web-1 isn't reporting.** No numbers have arrived in the last minute. The charts still show
  what arrived before. Check that the server runs this release and can reach Cloud.
- **No readings.** The range holds no numbers at all, as for a service deployed seconds ago.

A missing number shows as not measured, never as zero.

## Good to know

- **Cloud keeps metrics for 30 days.** A server you delete leaves its history in Cloud until it
  ages out. A server holds numbers it couldn't send for a while and sends them when Cloud is
  back, but a long outage leaves a gap in the charts. The gap shows as **Not measured**.
- **A server on an older Ployz sends no metrics.** Upgrade it to see its numbers. See
  [Upgrade Ployz on a server](../servers/manage-servers.md#upgrade-ployz-on-a-server).
- **Plain Docker volumes aren't measured**, and neither are files a service writes outside its
  volumes. Use a managed volume to see its usage.
- **Readings from before the 0.4 release are gone.** Servers kept their own history before, and
  0.4 removes it.
