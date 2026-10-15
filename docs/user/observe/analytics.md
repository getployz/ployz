---
title: Analytics
description: Count visitors, pages, sources and goals for each Environment, without cookies or a script.
---

Ployz counts the pages your Environment's domains serve, on the servers that serve them. There is
no script to add and no cookie banner to show: a visit is a browser asking for an HTML page.

Analytics comes with the paid plan on Ployz Cloud. A self-hosted Cloud runs it once its operator sets
`ANALYTICS_ENABLED=true` and restarts it.

## Turn it on

1. Open the Environment and click **Analytics**.
2. Click **Turn on**. It takes an owner or admin of the Organization.

Ployz counts from that moment, in your browser's time zone. The zone stays the Environment's for
good, so its days never shift under the numbers. Until the first visit lands, the page says so. If
visits arrived but none could be counted, it names the days they were lost on instead.

## Read the page

Pick **Today**, **7 days** or **30 days**, or a **From** and **Until** day. The cards show
**Visitors**, **Pageviews**, **Views per visit**, **Bounce rate** and **Visit duration** for the
range; the chart shows visitors and pageviews per hour for one day, per day for longer.

Below come the top **Pages**, **Sources**, **Entry pages**, **Campaigns** (`utm_campaign`),
**Languages** and **Hosts**. Click a page to see only visits that reached it; click **Page is**
to flip the filter to every other page, and the cross to clear it.

A dashed stretch of the chart is a partial day. That is today, whose visits are still arriving; a
day on which some visits could not be counted; or a recent day whose visits may still arrive late,
because a Machine held them while it could not reach Cloud.

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
- **Bots are left out.** Requests from known crawlers and from clients with no user agent are not
  counted.
- **History.** The page reads back 13 months. An older range starts at the oldest day kept, and
  the page says so.
