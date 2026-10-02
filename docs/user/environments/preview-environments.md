---
title: Preview environments
description: Give every pull request a temporary copy of your app at its own https address.
---

Each pull request gets a temporary copy of your app at its own https address. Every push to the
pull request redeploys it. When the pull request merges, production deploys the merge as usual,
with any setting changes you saved from the preview, and the copy and its data are deleted.
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
you change, save and close it like any other branch.

## Save changes so they go live with the merge

Code reaches production through Git. Setting changes made in the preview, like a new variable or
a new service, don't. Save them:

1. Open the preview and click the branch button at the top right of the canvas. The panel shows
   **3 changes to save**.
2. Click **Save to production**.
3. Check each change, as for any [save](environments.md#save-the-change-to-production). Turn on
   **Shut down pr-142 now** if you're done testing.
4. Click **Save to production**.

Production doesn't change yet. Its bottom bar shows **PR #142**, "3 changes · go live when #142
merges", and the merge deploys the code and these settings together. If production doesn't deploy
on push, they wait there as staged changes.

- **Changed the preview again?** Click **Save again**.
- **Changed your mind?** Click **Undo**. Closing the pull request without merging, or changing
  its base branch, also drops the save.
- **Production changed the same setting meanwhile?** Production keeps its value. Its **Details**
  show the pull request's value tagged **PR #142**, with **Use** to take it.

The changes go to every environment that deploys the pull request's base branch, which may not be
the one the preview started from. The panel shows a line for each.

Ployz also posts a **Ployz · ready to merge** check on the pull request. It asks for action, like
`3 changes to save in Ployz`, while the preview has changes you haven't saved. To block merging
until they're saved, make it a required check in your repository's branch protection rules.

## Shut down a preview

A preview you're not testing can come off your servers and keep its settings.

1. Open the preview, click the branch button, then ⋮.
2. Click **Shut down until the next push**. If it has data, type `my-app/pr-142` to confirm.

Its services stop and its own data is deleted. Its settings, saved changes and pull request stay.
The next push brings it back, or click **Deploy pr-142**. It starts fresh: its databases start
empty and setup commands run again.

To delete it instead, click ⋮, then **Close pr-142…**, type `my-app/pr-142` and click **Close
branch**. If the pull request is still open, its next push makes a new one.

**Next:** [get ready for production](../production-checklist.md).
