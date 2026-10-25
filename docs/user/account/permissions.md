---
title: Permissions
description: Who can do what in Ployz. Roles, tokens, Protected environments and approvals in one place.
---

Three things decide what you can do in Ployz: your role, the token you use, and whether the
environment is Protected. You get the smallest of the three. A token never does more than your
role, and a Protected environment holds some changes back even from a token that could deploy.

## Pick a role for each person

Everyone in an organization is an **Owner**, **Admin** or **Member**. Members deploy, stage
changes and answer approvals. Admins also invite people, manage servers and delete projects.
Owners also manage billing. The full table is under
[Invite people](organizations.md#invite-people).

When your role can't do something, Ployz says who can, such as "Only an owner or admin of this
Organization can do this."

## Narrow a token to one job

A token acts as you, narrowed by its preset:

- **Full as you** does everything you can.
- **Read only** reads and changes nothing.
- **Deploy** reads what you can read, but stages and deploys changes only in the environment you
  name, in every project that has one: a Deploy token for `production` deploys to each project's
  `production`. The dialog lists those projects next to each environment. It can't take anything
  off your servers: a deploy that removes a service, volume or domain, or `ployz env shutdown`,
  needs a **Full as you** token. So does a deploy that
  [prunes](../deploy/deployments.md) a service running in the environment that the environment
  doesn't hold. Use `ployz deploy --no-prune` to keep it.

Give CI a Deploy token for the environment it ships to. To keep one project's CI out of another
project, give that environment a name no other project uses. An owner's or admin's Deploy token
deploys into a Protected environment without waiting for another admin. See
[Make an organization token](../cli/overview.md#make-an-organization-token). An agent you connect
with `ployz mcp login` gets its own token, and that token can't answer approvals.

## Protect production

A Protected environment keeps its riskiest changes for owners and admins. Only they can connect a
repository, change the deploy branch, turn protection off, or make a Deploy token for it. A new
project's default environment starts Protected. See
[Protect an environment](../environments/environments.md#protect-an-environment).

A member can still ask. Their **Deploy** button reads **Request deploy**, and the request waits for
an owner or admin who didn't ask.

A merge into a Protected environment's deploy branch still deploys. Use GitHub branch protection to
decide who can merge. See [Protected environments](../deploy/github.md#protected-environments).

## Answer approvals

Some changes wait for a person before they run. The most common are:

- every deploy an agent starts;
- anything an agent or the CLI does that would delete a service, volume or domain;
- a member's request in a Protected environment;
- removing a server or the organization, taking a project or environment off your servers,
  turning protection off, and an agent's stop or restart.

A delete from the CLI asks in your terminal first.
[Destructive actions ask first](organizations.md#destructive-actions-ask-first) has the full
list. The rest wait under **Waiting on you** in the [Ployz agent](agent-sidebar.md#approve-or-deny-a-deploy).
Read the card, then click **Approve** or **Deny**. An agent you connected to Ployz Cloud waits the
same way. See [Approve a deploy](connect-an-agent.md#approve-a-deploy).

## Good to know

- Nobody approves their own request, and an agent can't approve its own deploy.
- Protected guards what runs through Ployz. An admin with a shell on a server can still reach any
  environment on it.
- Leaving an organization ends your access and your tokens at once.

**Next:** [protect your production environment](../environments/environments.md#protect-an-environment).
