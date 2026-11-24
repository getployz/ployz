---
title: Permissions
description: Who can do what in Ployz. Roles, tokens, Protected environments and who says yes, in one place.
---

Three things decide what you can do in Ployz: your role, the token you use, and whether the
environment is Protected. You get the smallest of the three. A token never does more than your
role, and a Protected environment holds some changes back even from a token that could deploy.

## Pick a role for each person

Everyone in an organization is an **Owner**, **Admin** or **Member**. Members deploy, stage
changes and approve requests. Admins also invite people, manage servers and delete projects.
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
with `ployz mcp login` gets its own token, and that token can't approve anything.

## Protect production

A Protected environment keeps its riskiest changes for owners and admins. Only they can connect a
repository, change the deploy branch, turn protection off, or make a Deploy token for it. A new
project's default environment starts Protected. See
[Protect an environment](../environments/environments.md#protect-an-environment).

A member can still deploy, stop and restart there. Each waits for an owner or admin who didn't ask.
Anything else only an owner or admin can do is refused with **An owner or admin does this.**

A merge into a Protected environment's deploy branch still deploys. Use GitHub branch protection to
decide who can merge. See [Protected environments](../deploy/github.md#protected-environments).

## Say yes to what removes something

A change that would delete a service, volume or domain, remove a server or the organization, take
a project or environment off your servers, or turn protection off shows what it removes first.

- In a terminal, `ployz` asks yes or no. In a script or with `--json`, it changes nothing and
  names the command to run again with `--approval`.
- In the dashboard, confirming the dialog is your yes.
- An agent's deploy, stop or restart comes back as a link. Open it, read what it does, and approve
  or deny. Any other removal from an agent is refused with where a person does it.

[Destructive actions ask first](organizations.md#destructive-actions-ask-first) has the full list.

## Good to know

- Nobody approves their own request, and an agent can't approve anything.
- Protected guards what runs through Ployz. An admin with a shell on a server can still reach any
  environment on it.
- Leaving an organization ends your access and your tokens at once.

**Next:** [protect your production environment](../environments/environments.md#protect-an-environment).
