---
title: Analytics
description: Count visitors, pages, sources and goals, and read each route's speed and failures, for each Environment, without cookies or a script.
---

Ployz records every request your Environment's domains serve, on the servers that serve them, and
sends it to Cloud as a request row. There is no script to add and no cookie banner to show. A visit
is a browser asking for an HTML page, and the same rows show how fast each route answers and which
requests failed.

Every Ployz Cloud Organization has analytics. A self-hosted Cloud runs it once its operator sets
`ANALYTICS_ENABLED=true` and restarts it.

## Turn it on

1. Open the Environment and click **Analytics**.
2. Click **Turn on**. It takes an owner or admin of the Organization.

Ployz counts from that moment, in your browser's time zone. The zone stays the Environment's for
good, so its days never shift under the numbers. Until the first visit lands, the page says so. If
visits arrived but none could be counted, it says no visits were counted.

## Read the page

Pick **Web**, **API** or **Errors** at the top of the page. **Web** counts visits, the other two
read every request.

## Web

Pick **Today**, **7 days** or **30 days**, or a **From** and **Until** day. The cards show
**Visitors**, **Pageviews**, **Views per visit**, **Bounce rate** and **Visit duration** for the
range; the chart shows visitors and pageviews per hour for one day, per day for longer.

Below come the top **Pages**, **Sources**, **Entry pages**, **Campaigns** (`utm_campaign`),
**Languages** and **Hosts**. Click a page to see only visits that reached it; click **Page is**
to flip the filter to every other page, and the cross to clear it.

A dashed stretch of the chart is a partial day. That is today, whose visits are still arriving; a
day on which some visits could not be counted; or a recent day whose visits may still arrive late,
because a Machine held them while it could not reach Cloud.

## API

**API** lists each route your services answered, with its **Requests**, **Errors** (the share
that answered 5xx), and the **p50**, **p95** and **p99** time to answer. A route is the
path with its ids, dates and content hashes replaced, so `/users/42` and `/users/7` both read
`/users/:id`. Click a column to sort by it.

**Slowest** lists the 20 slowest requests with their time, method, path, status, duration and
request id. Search your logs for the request id to see what that request did.

Pick **1h**, **6h**, **24h**, **7d** or **30d**, and a service, or **All services**.

## Errors

**Errors** charts the requests that answered 4xx or 5xx, per hour. **By route** counts them by route
and status, and **Newest** lists the latest with their request ids. It takes the same window and
service as **API**.

## Goals

A goal counts the visitors who reached a path. Under **Goals**, give it a name and a path such as
`/signup/done`. A `*` matches any run of characters, so `/signup*` counts `/signup` and
`/signup/done`. **Rate** is the goal's visitors over all visitors in the range.

## Good to know

- **Visitors are counted per day.** Ployz tells visitors apart by a hash that changes every day and
  keeps no address. A visitor who comes back the next day counts again, and a visit that runs past
  midnight counts as two.
- **Single-page apps count their first page only.** Moving between routes in the browser asks the
  server for no HTML page, so Ployz never sees it.
- **What leaves your servers.** Each request row carries its time, service, host, method, path,
  status, duration, size, request id, referring host, language, UTM tags, user agent and the
  daily visitor hash. The visitor's IP address never leaves the server.
- **Bots are left out.** Cloud checks each request's user agent against a list of known crawlers
  and tools, and keeps only the name it matched. Bot requests count for no visit and show in neither
  **API** nor **Errors**. Cloud keeps no user agent.
- **History.** Cloud keeps request rows 30 days. An older range starts at the oldest day kept, and
  the page says so.
