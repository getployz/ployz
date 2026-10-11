---
title: Preview environments
description: Give every pull request a temporary copy of your app at its own https address.
---

Each pull request gets a temporary copy of your app at its own https address. Every push to the
pull request redeploys it. When the pull request merges, production deploys the merge with the
setting changes the preview made, and the copy and its data are deleted.
Closing the pull request without merging deletes it too.

The dashboard calls these PR environments.

## Turn on preview environments

Previews work for services that [deploy from GitHub](../deploy/github.md) through the Ployz
GitHub App. Until one does, **Settings → Project** has no **PR environments** section. (The
quick start's example deploys without the app, so connect GitHub and
[deploy your own app](../deploy/github.md) first.) The app
also needs **Pull requests: read** and **Checks: write** on the repository: if your repository
shows **Needs permissions approved on GitHub**, open it, click **Approve on GitHub**, and have the
installation's owner approve them.

1. Open **Settings → Project**, and under **PR environments** click your repository.
2. Click **Enable PR environments**. Under **Start from**, pick the environment each pull request
   starts from, usually production.
3. Under **Services**, click each service a pull request should get its own copy of, like
   `postgres`. Services that deploy from this repository always run the **PR's code**.
4. If pull requests get a new, empty database, add a **Setup command** to seed it, like
   `pnpm db:seed`.

<!-- screenshot: the PR environments panel over production's canvas, Start from set, web on PR's code and postgres Separate -->

The panel saves as you go. Pull requests already open get a preview on their next push.

> [!WARNING]
> A service you don't copy, usually a database, is shared with production and shows
> **production's** in amber with a warning sign. Every pull request's code reads and writes that data. Click it and choose
> **Separate postgres** unless that's what you want.

Two more switches sit in the panel. **Delete when the PR closes** is on by default. **Enable bot
PR environments** is off: turn it on to preview pull requests from Dependabot, Renovate and other
bots.

## What each pull request gets

When a pull request opens, Ployz deploys its preview, named like `pr-142`:

- Your repository's services run the pull request's code. Every service in the preview runs one
  replica.
- Each copied service with a generated address gets its own. If production's `web` is
  `web.acme-x7q2.ployz.app`, the preview's is `web-pr-142.acme-x7q2.ployz.app`. Custom domains
  aren't copied.
- Each push redeploys it, even if production doesn't deploy on push. **Wait for CI** and **Watch
  paths** still apply.

Open previews are listed under **Open now** in the panel, and in the environment switcher under
the environment they start from. Each one is a [branch](environments.md) of that environment, so
you change, sync and close it like any other branch.

## Config changes in a pull request

Code reaches production through Git, and so do the preview's setting changes, like a new variable
or a new service: when the pull request merges on GitHub, every change the preview made since it
started lands in production.

To see what will land, open the preview and click **On merge · 3** at the top right of the canvas.
The list reads `#142 → production`, one row per change: the service, the setting, then
production's value → the preview's, like `CACHE_TTL 30 → 60`. In a row's ⋯ menu:

- **Edit** sets the value production gets instead of the preview's. The row shows **Edited** and
  the preview's own value under it; **Reset** in the ⋯ menu goes back to the preview's.
- **Exclude** keeps the change in the preview only. The row stays in the list, greyed, marked
  **Excluded**; click **Include** to put it back.

A setting production changed after the preview started reads `production 45 → 60`: the preview's
value wins when it merges. A secret the preview added that production has no value for reads
**Not set in production**. Set it in production, or with **Edit**, before you merge; otherwise
production's deploy skips it, and any variable that references it, and warns. Removed variables
and settings ride too; a removed service doesn't. Replicas, CPU and memory limits, and custom domains never do.

Ployz posts a **Ployz · ready to merge** check on the pull request that mirrors the list, like
`3 config changes · TOKEN not set`. It's green, or neutral when a secret is Not set or production
changed the same setting. It never fails, so it never blocks a merge. Its **Details** link opens
the list.

When the pull request merges, production gets one saved change, **Merged #142 · 3 changes**, and
the merge commit's push deploys it with the code. To take it back, click **Undo** beside it in
production's **History**.

To take the changes before the merge, click **Sync · 3**, check each change as for any
[sync](environments.md#sync-the-change-to-production), and click **Sync 3 changes**. They become
production's changes to deploy now, and leave the On merge list. From the CLI, `ployz env sync --to
production` from the preview does the same.

## Shut down a preview

A preview you're not testing can come off your servers and keep its settings.

1. Open the preview and open the Sync button's ▾ menu.
2. Click **Shut down until the next push**. If it has data, type `my-app/pr-142` to confirm.

Its services stop and its own data is deleted. Its settings, synced changes and pull request stay.
The next push brings it back, or click **Deploy pr-142**. It starts fresh: its databases start
empty and setup commands run again.

To delete it instead, open the ▾ menu, then **Close pr-142…**, type `my-app/pr-142` and click **Close
branch**. If the pull request is still open, its next push makes a new one.

**Next:** [get ready for production](../production-checklist.md).
