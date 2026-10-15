---
title: Preview environments
description: Give every pull request a temporary copy of your app at its own https address.
---

Each pull request gets a temporary copy of your app at its own https address. A member's pushes
redeploy it. Anyone else's pull request waits for a member to approve it. When the pull request merges, production deploys the merge with the
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

## Who deploys automatically

A preview runs the pull request's code with copies of its environment's secrets. So Ployz deploys
a pull request on its own only when a member of your Ployz organization wrote it, from a branch
of the repository itself. A pull request from a fork, or from someone outside your organization,
reads **Needs approval** under **Open now**, with its commit like `a1b2c3d`. Until a member
approves it, nothing builds and no preview exists.

Ployz knows a member by the GitHub account they signed in with. A member who signed in some other
way has their pull requests wait too.

To deploy it, read the pull request's changes on GitHub, then click **Approve deploy** beside it.
From the CLI, run `ployz env pr approve 142`. The approval covers that one commit. A new push
reads **Needs approval** again, with the new commit, and the preview keeps running the commit you
approved until you approve the new one. If someone pushed after you loaded the page, Approve deploy
is refused with **This pull request has a newer commit. Approve that one.**

Bots' pull requests from the repository itself count as a member's once **Enable bot PR
environments** is on. A bot's pull request from a fork still needs approval.

`ployz env pr` lists each open pull request with where it stands, like `#142 needs approval
a1b2c3d` or `#140 deploys`.

## What each pull request gets

When a pull request opens, Ployz deploys its preview, named like `pr-142`:

- Your repository's services run the pull request's code. Every service in the preview runs one
  replica.
- Each copied service with a generated address gets its own. If production's `web` is
  `web.acme-x7q2.ployz.app`, the preview's is `web-pr-142.acme-x7q2.ployz.app`. Custom domains
  aren't copied.
- A push deploys the new commit, even if production doesn't deploy on push, once the pull request
  [deploys automatically](#who-deploys-automatically) or a member approves that commit. **Wait
  for CI** and **Watch paths** still apply.

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
and settings ride too. Replicas, CPU and memory limits, and custom domains never do.

A service or config the preview removed reads **Removes db**. If production changed it since the
preview started, the row adds **Changed there**; the merge removes it anyway. If something in
production still uses it, like a variable that references `db`, the row reads **In use** with what
uses it, and the merge leaves it in production while the rest lands. A removed volume reads
**Kept**: the merge never removes a volume or its data, and production's services keep their mounts
on it unless the preview removed the service too. Remove it in production yourself when
you're sure. Only what the preview copied can be removed this way; a service the preview never
had stays in production untouched.

Ployz posts a **Ployz · ready to merge** check on the pull request that mirrors the list, like
`3 config changes · TOKEN not set`. It's green, or neutral when a secret is Not set or production
changed the same setting. It never fails, so it never blocks a merge. Its **Details** link opens
the list. A pull request into a branch no Environment deploys from reads **Nothing deploys from
release**: its changes stay in the preview.

When the pull request merges, production gets one saved change, **Merged #142 · 3 changes**, and
the merge commit's push deploys it with the code. To take it back, click **Undo** beside it in
production's **History**.

The merge deploys every service it changed: one whose own settings changed, one that mounts a
changed config, and one that reads a changed variable. It does so even for a service with
auto-deploy off or one that doesn't build from the branch, which redeploys with the code it last
built. An edit you staged in production but didn't save stays staged.

A removal deploys too: the service's containers stop and go away, and its volumes stay on the
Machine with their data. The merge opens no approval, because it never deletes volume data.
**Undo** in production's **History** brings the service or config back whole, settings and
secrets included, and the next deploy starts it on the same volumes. If another service took its
name since, Undo is refused with **api exists. Rename it first.**

If the merge couldn't land in production, or landed but couldn't deploy, the check says so, like
**Didn't land in production** or **Saved in production, not deployed**, with the reason. A preview
whose changes didn't land stays open, so nothing it held is lost.

To take the changes before the merge, click **Sync · 3**, check each change as for any
[sync](environments.md#sync-the-change-to-production), and click **Sync 3 changes**. They become
production's changes to deploy now, and leave the On merge list. From the CLI, `ployz env sync --to
production` from the preview does the same.

## Shut down a preview

A preview you're not testing can come off your servers and keep its settings.

1. Open the preview and open the Sync button's ▾ menu.
2. Click **Shut down until the next push**. If it has data, type `my-app/pr-142` to confirm. A
   preview with no data shuts down without asking.

Its services stop and its own data is deleted. Its settings, synced changes and pull request stay.
The next push brings it back, once it [deploys](#who-deploys-automatically), or click **Deploy pr-142**. It starts fresh: its databases start
empty and setup commands run again.

To delete it instead, open the ▾ menu, then **Close pr-142…**, type `my-app/pr-142` and click **Close
branch**. If the pull request is still open, its next push makes a new one.

**Next:** [get ready for production](../production-checklist.md).
