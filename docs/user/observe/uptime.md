---
title: Uptime
description: See whether visitors can reach each service, check it after a deploy, and read its incidents.
---

Ployz checks every service with a public address from outside your servers, the way a visitor
reaches it. When the check can't get through, the service shows a red ⚠ and an incident opens.

## See a service's uptime

1. Click the service on the canvas.
2. Open its **Monitoring** tab.

The status line reads **Up**, **Unreachable** or **Checking**, with the share of checks that
passed. **Response time** and the history strip cover the last hour. Pick a quick range from
5 minutes to 30 days, or type a **From** and **Until**. Click an incident to see what each
check got back.

<!-- screenshot: the Monitoring tab of web, Up with its response time chart -->

## Check right after a deploy

Ployz checks the services a deployment applied as soon as it succeeds, so a deploy that broke
the address shows **Unreachable** within seconds. Between deploys, it checks every 5 minutes.

To check now, click **Check now** on the **Monitoring** tab. The button reads **Checking** until
the result is in, and the status line moves with it.

## From the CLI or an agent

On a Ployz Cloud backend, `ployz uptime show` prints what the **Monitoring** tab shows: the
status, the share of checks that passed, what each region's latest check got back, its last
error, and the incidents in the range.

```sh
ployz uptime show web
ployz uptime show web --project shop --environment staging --from 6h
```

`--project` and `--environment` default to your default Project and its default Environment.
`--from` and `--to` take an offset back from now such as `15m`, `6h` or `7d`, or a UTC instant.
The range is the last hour when you leave them out.

`ployz uptime check` does what **Check now** does, waits for the check to answer, and prints
what each region got back. It changes no setting. If the check hasn't answered within about 10 seconds, it says
so, and `ployz uptime show` shows the result once it lands.

```sh
ployz uptime check web
```

The sidebar and `ployz mcp` offer both as `uptime_show` and `uptime_check`, so an agent can see
that a service answers 500 and check it again after a fix. A service with no public address
answers `not_checked`.

## Good to know

- **What gets checked.** The service's generated domain, or its first custom domain when it has
  none, at its [health check](../deploy/deployments.md#deploy-without-downtime) path, or `/`
  without one. At the path, any answer below 400 counts as up. At `/`, any answer below 500
  counts, so a 404 still shows the service is up. A service with no public address shows
  **Not checked** and **Add domain**.
- **A 4xx points at the path.** If the check has never passed and your app answers its health
  check path with a 4xx, the **Monitoring** tab shows the **Healthcheck path** field with what
  the app answered. Change it there, then deploy. The check uses the new path from then on.
- **A failed deployment isn't checked at once.** The regular check still runs, against what
  that deployment did apply.
- **Uptime counts every check.** The ones after a deploy and from **Check now** count toward the
  percentage and the charts, like the regular ones.
