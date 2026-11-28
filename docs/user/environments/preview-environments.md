---
title: Preview environments
description: Deploy any pull request as a copy of staging, at its own https address.
---

A preview is a copy of one of your environments, usually staging, running a pull request's code
at its own https address. Nothing deploys until someone asks: each pull request gets a
**Deploy as a preview** link, and one click from a member of your organization makes the copy.
After that, every push to the pull request redeploys it. When the pull request closes, the
preview and its data are deleted.

A pull request's preview is either **Live**, with its address, or **Not deployed**, with the
reason and what to do about it. There's nothing in between.

## Set up previews

Previews work for services that [deploy from GitHub](../deploy/github.md) through the Ployz
GitHub App, and they need a [passkey](../servers/manage-servers.md#add-a-passkey) your servers
trust. Your project also needs a Protected environment, usually production, so Ployz knows which
one previews must never copy.

1. Open any environment of the project and click **Pull requests** in the left rail, then
   **Preview settings**.
2. Under **Each preview is a copy of**, pick the environment previews copy, like `staging`.
   Production and other Protected environments are never offered: a pull request's code that
   nobody has reviewed yet runs with this environment's secrets. Branches aren't offered either,
   so make the environment with **New environment**, not as a branch of production. A pull
   request into the Git branch that environment deploys gets no preview and reads
   **Not deployed: it merges into staging's own branch**, so have it deploy another branch, like
   `dev` while pull requests go into `main`.
3. Under **Where previews run**, check the servers previews may use. Each preview runs on one of
   them.
4. Under **Live previews at once**, pick how many previews can be live together, from 2 to 12.
5. Click **Turn on with passkey**. The sheet lists what you're approving: who can deploy, which
   environment's secrets each preview gets, by name, where previews run and how many, and which
   services copy. Your passkey confirms it once, and your servers check it themselves.

<!-- screenshot: Preview settings with staging, two servers and Up to 5 -->

Every open pull request of the repository now has a **Deploy as a preview** link.

To change a setting later, click **Edit**. Before you sign again, the sheet shows what changes,
like `Up to 5 → 8 live previews`. Live previews pick up the change at their next deploy.

To stop, click **Turn off** and confirm with your passkey. Every live preview closes, its data is
deleted, and the **Deploy as a preview** links go away. The pull requests stay open.

## Deploy a pull request

Ployz posts one comment on each open pull request: **Deploy as a preview**. The link opens a page
in Ployz that shows what will deploy before anything does:

- the pull request and its latest commit
- the environment it copies, like `staging`, with its services
- the server it runs on, which Ployz picks when it deploys
- **Its databases start empty**

Click **Deploy**. The preview builds and starts, and the comment on GitHub changes to **Open
preview** with its address. The same page, and **Pull requests** in Ployz, show it as
**Deploying**, then **Live at** its address.

You can also deploy from **Pull requests** in Ployz: each row has a **Deploy** button.

Every push to the pull request redeploys its live preview.

Each push to a pull request starts a **Ployz build** run on GitHub. While the pull request has no
live preview, Ployz has nothing for that run to build, and GitHub marks the run failed. Deploying
the preview runs it again.

If someone pushed after you opened the page, Deploy is refused with
**This pull request has a newer commit. Reload to deploy it.**

### Who can deploy

Any member of your Ployz organization can deploy a pull request that a member opened from a
branch of the repository itself. Ployz knows a member by the GitHub account they signed in with,
so to let a contributor's pull requests deploy, [invite them](../account/members.md) to your
organization.

A pull request from a fork, or from someone outside your organization, reads
**Not deployed: rdev0x isn't in your organization on Ployz**. Invite them, or deploy it from a
branch of your own.

## What each preview gets

A preview is a copy of the environment you picked, as it was last saved:

- **Your repository's services run the pull request's code.** Every other service runs as the
  environment you copied does, from the same image.
- **The copied environment's secrets.** Each service gets the same variables and secret values as
  in staging, as they were when you set up previews.
- **Empty databases.** Each volume starts new and empty, and nothing is copied or seeded into it.
  Your app has to cope with an empty database, for example by running its migrations at start.
- **One replica** of each service, and no custom domains.
- **Its own address.** Each service with a generated address gets its own, named after the pull
  request, like `web-3-pr-12.acme.ployz.app`. The comment and **Pull requests** link to it.

A preview can't reach your other environments' services or data, or another preview's.

## The limit

When as many previews are live as **Live previews at once** allows, a new deploy is refused with
**Not deployed: 5 of 5 previews are live**. Nothing waits in line, and nothing deploys later on
its own.

- **Close one** lists the live previews by their last push, each with a **Close** button. Close
  one, then deploy again.
- **Raise the limit** opens Preview settings, where a higher limit takes one passkey signature.

The limit counts every live preview of the repository, on all the servers previews run on.

## Close a preview

A preview closes on its own when its pull request merges or closes, and the comment on GitHub
changes to **Preview closed**.

To close one sooner, open it from **Pull requests** and click **Close preview**. Its copy and
databases are deleted. The pull request stays open, so you can deploy it again later, and it
starts with empty databases.

## What previews don't cover

When you turn previews on, you approve your services as they're set up in the environment
previews copy, secrets included. A preview always runs those settings: settings you change in a
preview itself don't deploy.

If you later change the copied environment, like a variable, a secret, an image or a new
service, new deploys read **Not deployed: changes web's settings, which the policy doesn't
cover**. Live previews keep running. Open **Preview settings**, click **Edit**, and save with
your passkey to cover the new settings.

**Next:** [get ready for production](../production-checklist.md).
