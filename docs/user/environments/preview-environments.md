---
title: Preview environments
description: Give every pull request a temporary copy of your app at its own https address.
---

Each pull request gets a temporary copy of your app at its own https address. Every push to the
pull request redeploys it. When the pull request merges, production deploys the merge as usual,
with any setting changes you synced from the preview, and the copy and its data are deleted.
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

## Queue changes for production until the merge

Code reaches production through Git. Setting changes made in the preview, like a new variable or
a new service, don't. Sync them:

1. Open the preview. The Sync button at the top right of the canvas reads **Sync to production ·
   3**. Click it.
2. Check each change, as for any [sync](environments.md#sync-the-change-to-production). The dialog
   says "Production can apply these once #142 merges."
3. Click **Sync 3 changes**.

Production doesn't change yet: the changes are **queued** there. The toast says **Queued for
production**, the preview's button reads **Queued**, and production's bottom bar shows **PR #142 ·
Queued**. From the CLI, `ployz env sync --to production` from the preview prints the same: "Queued
for production: apply it in Changes once PR #142 merges." A sync that asks to land now, like the
SDK's `when: now`, is queued the same way until #142 merges into a branch production deploys, and
`--close` is refused until then: `#142 isn't merged: its Sync is queued, so sync without --close`.

To apply the changes, open production's bottom bar and click **Details**. **Queued** lists
`pr-142 · 3 changes` with **Apply**. Click it, and the changes move to **Included** as production's
changes to save or deploy. Each pull request's row says where it stands:

| Label | Meaning |
|---|---|
| **Waiting for #142** | The pull request is still open. |
| **Merged** | It merged into a branch production deploys. |
| **Closed** | It closed without merging. |
| **Merged into release** | It merged into `release`, a branch production doesn't deploy. |

**Save** and **Deploy** wait until every included pull request is **Merged**: they're turned off
and say **Waiting for #142**. Remove a row that won't get there to save the rest. `ployz env
publish` and `ployz deploy` refuse the same way, with `#142 isn't merged: Remove it to save the
rest.`

- **Changed the preview again?** Sync again. Before you click **Apply**, the new sync replaces
  the queued one. After, it updates production's changes at once.
- **Changed your mind?** Click **Undo** in the toast, or **Undo sync to production** in the Sync
  button's ▾ menu. In production, **Remove** in the row's ⋮ menu drops it too, in one click.
  Closing the pull request or changing its base branch keeps it queued: the row then reads
  **Closed** or **Merged into** the other branch.
- **Production changed the same setting meanwhile?** **Apply** keeps production's value for that
  setting. Its **Details** show the pull request's value, "PR #142: …", with **Use** to take it.
- **Added a secret?** Its value stays in the preview. Type production's value in the secret's row
  of the Sync dialog, and it is queued with the changes. If you didn't, **Apply** asks for it: type
  it into **Set value** and click **Apply** again. A value is never shown back, only **Value set**.

The changes go to the environment that deploys the pull request's base branch, which may not be
the one the preview started from. Where several do, pick one from the Sync button's ▾ menu.

Ployz also posts a **Ployz · ready to merge** check on the pull request, and the Sync button's ▾
menu shows it. It asks for action, like `3 changes to sync in Ployz`, while the preview has
changes you haven't synced. To block merging until they're synced, make it a required check in
your repository's branch protection rules.

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
