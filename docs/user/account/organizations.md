---
title: Organizations and billing
description: Your organization, its settings, deleting it, and what Pro costs.
---

An organization holds your projects, your servers and your billing. The first time you sign in,
Ployz creates one for you, named after you (like Ada's Projects).

## Your organization

Ployz is built for one person today. You can't invite anyone to your organization yet, so there
are no teams or roles. To let a CI job act for you, make an
[organization token](../cli/overview.md#make-an-organization-token).

If you have more than one organization, switch between them under **Switch organization** in the
account menu.

In the sidebar, click **Organization** for its settings:

- **General**: your organization's **Domain**, which your services' free addresses live under.
  It appears with your first deploy. Below it, **Ask before destructive actions** (see below),
  and **Forget all servers** for when your servers were deleted.
- **Builds**: where builds run. See [Where builds run](../builds/where-builds-run.md).
- **Billing**: your plan.

![Organization → General: the domain and the Danger section](../images/organization-general.png)

## Ask before destructive actions

A coding agent working through the CLI can publish or deploy a change that deletes a running
service, a volume and its data, or a domain. With **Ask before destructive actions** on, Ployz
stops that command and waits for you to approve exactly what it would delete. It's on for every
organization unless you turn it off.

- Only `ployz publish` and `ployz deploy` ask, and only when the change deletes something that's
  running. Rolling out a new image or changing variables never asks.
- What you do in the dashboard never asks. Neither do settings edits.
- If the change moves on after Ployz asked, your approval no longer fits it, and the command asks
  again about the new one.

To turn it off, go to **Organization → General** and switch off **Ask before destructive
actions**.

This keeps a well-behaved agent from deleting something by mistake. It doesn't stop one that's
trying to get around it: anything that holds your login or token can approve its own request.
Keep tokens you give an agent short-lived, and keep
[backups](../services/databases.md#back-up-a-database) of data you can't lose.

## Billing

Ployz is free on your own servers: unlimited servers and projects, preview environments,
generated https addresses and custom domains. Your hosting provider bills you for the servers.

**Organization → Billing** shows the plans. **Hobby**, $5 a month, and **Pro**, $49 a month per
organization, are coming soon.

If you subscribed to the old $9 Pro plan for custom domains, you no longer need it. Click
**Manage billing** on the same page to cancel it or get invoices.

A [self-hosted Ployz Cloud](../self-hosting.md) has no billing, and custom domains are always
allowed.

## Delete an organization

Deleting an organization removes it with its settings and tokens, and disconnects your servers
from it. It can't be undone.

1. Delete every project in it. **Delete organization** stays disabled until you do.
2. **Cancel Pro first.** Deleting the organization doesn't cancel it: go to
   **Organization → Billing → Manage billing**.
3. Go to **Organization → General** and click **Delete organization**.
4. Type the organization's slug, as the dialog shows it, and click **Delete**.

<!-- screenshot: the Delete organization dialog with the slug typed -->

If a server is offline, the organization stays, disabled, until that server is back. Then click
**Try again**.

With no organization left, the dashboard offers **Create an organization**.

From the CLI, `ployz org rm SLUG` does the same. If this device was signed in to that
organization, it moves to another of yours and says which. With none left, run `ployz logout`,
then `ployz login`.
