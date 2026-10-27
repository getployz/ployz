---
title: Organizations and billing
description: Your organization, its settings, deleting it, and billing.
---

An organization holds your projects, your servers and your billing. The first time you sign in,
Ployz creates one for you, named after you (like Ada's Projects). An agent can also make one
before anyone signs in, named Agent sandbox. Open its claim link within 24 hours to keep it. See
[Try Ployz without an account](try-without-an-account.md).

## Your organization

You can invite people to work in your organization with you. See
[Invite people](#invite-people). To let a CI job act for you, make an
[organization token](../cli/overview.md#make-an-organization-token) under **Organization →
Tokens**. Pick **Deploy** and an Environment for a token that can only deploy there, **Read only**
for one that can't change anything, or **Full as you** for one that can do whatever you can.
**Deploy** needs an Environment, so in an organization without one the dialog starts at **Read
only**.

To make another organization, open the account menu and click **Switch organization → New
organization**. Name it, change its URL if you like, and click **Create**. You land in the new
organization, and you own it. Switch between your organizations under **Switch organization**.

From the CLI, run `ployz org new` with a name:

```sh
ployz org new "Acme Corp"
ployz org new "Acme Corp" --slug acme
```

The slug comes from `--slug`, or from the name when you leave it out. If another organization
already has that slug, Ployz adds a short suffix, and the command prints the slug it picked. The
CLI then acts in the new organization. Your browser and your other signed-in CLIs stay where they
were. An organization token can't make an organization, so sign in with `ployz login` first.

In the sidebar, click **Organization** for its settings:

- **General**: your organization's **Display name**. Owners and admins can change it: type a new
  one and press Enter or click the check. Its URL stays the same. Below it, your organization's
  **Domain**, which your services' free addresses live under.
  Until you generate your first domain, it reads **No domain yet**. Below it, **Allow support access**, and **Forget all
  servers** for when your servers were deleted.
- **Members**: who's in the organization, their roles, and, for owners and admins, pending
  invites. See
  [Invite people](#invite-people).
- **Tokens**: organization tokens. See
  [Make an organization token](../cli/overview.md#make-an-organization-token).
- **GitHub**: the connected GitHub App. See [Deploy from GitHub](../deploy/github.md).
- **Builds**: where builds run. See [Where builds run](../builds/where-builds-run.md).
- **Billing** (when billing is on): your plan.

![Organization → General: the domain and the Danger section](../images/organization-general.png)

## Invite people

Everyone in an organization has one role: **Owner**, **Admin** or **Member**. Each role can do
everything the role below it can, and more:

| Role   | Can do |
| ------ | ------ |
| Member | See the organization, its members, projects, servers, volumes, variables, logs and metrics. Create projects and environments, edit projects, stage changes, deploy, destroy and merge them, restart and stop services, answer approvals, and make their own organization tokens. |
| Admin  | Also change the organization's settings and support access, invite people, change roles and remove members, manage everyone's tokens and agents, connect GitHub, delete projects and environments, protect and unprotect environments, delete volume copies, and add, operate, open a shell on and remove servers. |
| Owner  | Also manage billing, delete the organization, and make or remove an owner. |

When you try something your role can't do, Ployz says who can: "Only an owner or admin of this
Organization can do this." or "Only an owner of this Organization can do this." A change to a
project's configuration that your role can't make is refused with the permission it needs, such as
`Needs environment:delete`; `ployz <command> --help` names that permission before you run it.

To invite someone, go to **Organization → Members** and click **Invite**. Pick **Member** or
**Admin**, add their email if you like, and click **Make link**. When Ployz Cloud has email on, it emails the
link to that address, and the dialog tells you where it went. A self-hosted Cloud without email,
or an invite with no email, sends nothing, so copy the link from the dialog and send it to them
yourself. Whoever opens the link, signs in and clicks **Join** and the organization's name joins with that role, so share it
only with the person you mean. A link works once and expires after 7 days. Someone already in
the organization who opens it sees **You're in** and **Open** instead, and keeps their role.
Owners and admins see pending invites, who sent them and when they expire. Click **Revoke** to
cancel one. The list updates as soon as someone joins.

Owners and admins change a role with the select beside each person, and click **Remove** to take
someone out. To leave an organization yourself, click **Leave** on your own row. An organization
always keeps an owner, so its last owner can't leave or step down until they make someone else
an owner. `ployz org member rm` refuses to remove the last owner, and `ployz org member role`
refuses to give them another role. Each refuses at once, before it asks for any approval.

When someone leaves or is removed, they lose the organization at once: its dashboard, their
organization tokens, and their signed-in CLIs' access to its servers. If they joined by invite and
open it again, it says their membership ended.

## Let Ployz support in

Turn on **Allow support access** under **Organization → General** to let Ployz support open your
organization and do what you can. It's off until an owner or admin turns it on, and turning it
off applies on support's next request. While support has your organization open, a banner at the
top says so. A [self-hosted Ployz Cloud](../self-hosting.md) has no Ployz support, so the setting
doesn't appear.

Support sees your projects, services and settings. Three switches under the setting decide what
more it sees:

- **Variables**: values of variables that aren't sealed. Off, they read as sealed.
- **Config files**: what's in each config file. Off, support sees each file's name and mount but
  not its contents.
- **Container logs**: what your containers print, including a failed health check's output.
  What a build prints is always visible.

All three start off, and turning support access off turns them off. Each applies on support's
next request. Support sees these switches but can't change them. It can still change anything
else you can, including a value it can't read, but it can't delete the organization.

## Destructive actions ask first

A coding agent working through the CLI can save or deploy a change that removes a running
service, deletes a volume and its data, detaches a volume from a service, or removes a domain. It
can also drain or remove a server, or clean up what a deleted environment left on your servers.
It can remove a project, an environment or the whole organization, forget every server, or turn
an environment's protection off. Ployz refuses that command with `approval_required`, naming what
it would remove and the approval it opened for you. Every organization works this way, and no
setting turns it off. The one exception is an [Agent sandbox](try-without-an-account.md) nobody
has claimed yet: no person could answer there, so nothing asks until someone keeps it.

How you answer depends on where the command runs:

- In a terminal, it lists what it would remove and asks you to type the environment's name. A
  server command asks for the server's name, or for `server clean` the name you passed it.
  `ployz org rm`, and `ployz server forget` with no server named, ask for the organization's
  slug. Type it and the command carries on. Anything else, or Ctrl-C, leaves the approval
  pending and changes nothing.
- Without a terminal, such as in a script, it prints the same list, the approval's ID, a link to
  the [Ployz agent](agent-sidebar.md) where it waits, and `ployz org approval approve ID`. With
  `PLOYZ_TOKEN`, when Cloud doesn't name the organization, the link is Ployz Cloud itself. A
  redeploy's list names the deploy it ships again, such as `This redeploy of #4:`, the later
  deploys it rolls back, and what it removes again that its deploy removed, such as
  `✕ removes Service extra`. When a [Protected environment](../environments/environments.md#protect-an-environment)
  asks, it prints Ployz Cloud's reason, that an owner or admin who didn't ask approves it in
  Ployz Cloud or with `ployz org approval approve ID`, and `ployz org approval show ID` to follow
  it, then waits for them. It
  waits until someone approves or denies it in Ployz Cloud, then carries on, or fails naming what
  was denied and the reason given. Ctrl-C, or losing Ployz Cloud, stops waiting and leaves the
  approval pending, and the error names the command to run again with `--approval ID`, except
  for `ployz env sync --value ROW` (see `--json` below), whose rerun asks again. If Ployz
  Cloud answers with an error instead, the command fails saying it couldn't check the approval,
  and names the command to run again only when the error is on Cloud's side. If Cloud refuses
  your sign-in or token, or you've left the organization, it names no command to run again;
  it says what to do instead, such as `ployz login`, a new token or `ployz logout`. Once you've
  left, `ployz logout` forgets the sign-in here though Cloud refuses to sign it out, and warns
  that Cloud didn't revoke it.
- With `--json`, it never asks or waits. It exits with `approval_required`, and `details.retry`
  is the same command with `--approval ID`. Run that once the approval is approved. The one
  exception is `ployz env sync --value ROW`, which refuses `--approval`, since an approval can't
  carry secrets given again on stdin. Its retry names each `--value ROW` but never the secret and
  no approval, so pipe the values on stdin again and it asks for a new approval. To finish such a
  sync, run it without `--json` so it waits for the approval.
- In an agent that uses `ployz mcp`, the agent's app asks you. In an app that can't ask, the
  call fails with a link, as for a connected agent below. See
  [Give the agent Ployz tools](../cli/overview.md#give-the-agent-ployz-tools).
- In the [Ployz agent](agent-sidebar.md) in the dashboard, you approve or deny right in its
  panel, and the agent waits for you.
- In an agent you [connected to Ployz Cloud](connect-an-agent.md), the call fails with a link.
  Approve or deny under **Waiting on you** in the Ployz agent, then have the agent try again.

- `ployz deploy` and `ployz deploy --skip-deploy` ask only when the change removes something that's running.
  Rolling out a new image or changing variables never asks.
- Every deploy or redeploy an agent asks for asks, even one that destroys nothing. That covers the
  [Ployz agent](agent-sidebar.md), `ployz mcp` and an agent you
  [connected to Ployz Cloud](connect-an-agent.md). An agent's save still asks only when it removes
  something, and one approval covers one deploy or redeploy. Outside a
  [Protected environment](../environments/environments.md#protect-an-environment), redeploying from the CLI or the
  dashboard never asks, except from the CLI when it takes something off: redeploying a
  `ployz env shutdown`, or rolling back to a deploy from before a service was added, asks as a
  deploy that removes the same would. In a Protected environment, a member's save, deploy or
  redeploy asks an admin even when it removes nothing. An owner's or admin's Deploy token there
  redeploys unasked when the redeploy destroys nothing. A rollback, or a redeploy that removes
  again what its deploy was approved to remove, asks an admin. One that would remove anything else
  or prune is refused, naming what and that an owner or admin can redeploy it.
- `ployz project rm`, `ployz env rm` and `ployz env shutdown` ask when they would take something
  off the servers. `ployz org rm`, `ployz server forget` with no server named and
  `ployz env protect --off` always ask.
- Removing or forgetting a server Ployz Cloud manages with `ployz server rm` or
  `ployz server forget SERVER` always asks. `ployz server drain`
  asks when a global service would run on no other server, and `ployz server clean` asks when it
  would stop a service or delete volume data. `ployz server upgrade` never asks for an approval.
- An agent's stop or restart of a service always asks, and one approval covers one stop or
  restart. Typed in a terminal, `ployz service stop` and `ployz service restart` don't ask yet.
  Starting a service never asks.
- Dashboard buttons and dialogs never ask: confirming a dialog is your approval. The exception
  is a member's request in a Protected environment, such as a save, deploy, sync, restart, stop
  or volume grow, which waits for an owner or admin (see [Permissions](permissions.md#protect-production)). When the
  [Ployz agent](agent-sidebar.md) does the same thing for you, it asks.
- If the change moves on after Ployz asked, your approval no longer fits it, and the command asks
  again about the new one.
- An approval covers one save. A later save that still carries a removal that hasn't
  deployed yet asks again.

A retry of a command you denied is still denied, even once it would destroy nothing. A retry of
one still waiting for your answer still waits, unless it no longer destroys anything.

An approval works only for the person who asked for it, from the same login or token they asked
from, and only for the command and the target they asked about. Anyone else who sends it is
refused with `forbidden`, and nothing changes. So is the same person from another login or
token, or a different command or target, even one that wouldn't ask for an approval at all, such
as a save into another environment that removes nothing, a quick `ployz volume sync`, or one
already done, such as `ployz env protect --off` for an environment that isn't protected. The
refusal says which. One exception: when an agent retries a tool that finds nothing left to write,
such as a save that already landed, only the person and the login or token are checked. Run the
command again without `--approval` to ask for an approval of your own. When two people, or one person from
two logins, ask for the same thing, each gets an approval of their own to answer.

An approval runs its command once. Sending it again for another run asks again, with a new
approval to answer, even when the command and target are the same, such as changing a member back
to a role you changed once already. A retry of the same deploy, or the same request sent again with
its request key, answers what the first one started and runs nothing new. A save or deploy that
Ployz refuses leaves its approval unused.

You can answer from a terminal too. `ployz org approval ls` lists the approvals waiting,
`ployz org approval show ID` shows what one would remove, and `ployz org approval approve ID` or
`ployz org approval deny ID --reason "keep the volume"` answers it. Approving asks you to type the
organization's slug, so it needs a terminal.

An agent can't answer an approval: `ployz mcp`, an agent you connected to Ployz Cloud and the
Ployz agent can neither approve nor deny one. When the Ployz agent or an agent you connected to
Ployz Cloud asked, you approve in the Ployz agent's sidebar, and `ployz org approval approve`
refuses with `forbidden`. When an agent using `ployz mcp` asked, you approve in its app's dialog,
in the sidebar, or with `ployz org approval approve`. The dialog and the command answer with this
computer's `ployz` login, so it must be signed in to the agent's Ployz Cloud and acting in the
organization that owns the request. Otherwise they refuse: run `ployz org use ORGANIZATION` to switch,
then answer again. If you run the same change yourself with `ployz`, typing the name in your terminal
approves it. Anyone in the organization can deny any request, from the dashboard or a terminal.

An agent's own sandbox doesn't ask. While only the agent owns it, nobody could answer, so its
deploys and removals go ahead. Once a person keeps it, the agent asks like anywhere else.

This keeps a well-behaved agent from deleting something by mistake. It doesn't stop a program
that's trying to get around it: anything that holds your own `ployz` login or one of your own
tokens can approve its own `ployz` request, as if you'd typed the name in a terminal. The agent
token `ployz mcp login` saves can't approve or deny anything.
Keep tokens you give an agent short-lived: a token can't make or revoke tokens, so revoking it
cuts the agent off. Keep
[backups](../services/databases.md#back-up-a-database) of data you can't lose.

## Billing

Ployz is free on your own servers: unlimited servers and projects, preview environments,
generated https addresses and custom domains. Your hosting provider bills you for the servers.

**Organization → Billing** shows the plans. **Hobby**, $5 a month, and **Pro**, $49 a month per
organization, are coming soon.

If you subscribed to the old $9 Pro plan for custom domains, you no longer need it. Click
**Manage billing** on the same page to cancel it or get invoices.

In an agent's sandbox, billing waits until someone keeps it.

A [self-hosted Ployz Cloud](../self-hosting.md) has no billing, and custom domains are always
allowed.

## Delete an organization

Deleting an organization removes it with its settings and tokens, and disconnects your servers
from it. It can't be undone. Only an owner can delete it. For anyone else **Delete organization**
is disabled.

1. [Delete every project](../environments/environments.md#delete-a-project) in it. **Delete
   organization** stays disabled until you do.
2. **Cancel any subscription first**, such as the old $9 Pro plan. Deleting the organization doesn't cancel it: go to
   **Organization → Billing → Manage billing**.
3. Go to **Organization → General** and click **Delete organization**.
4. Type the organization's slug, as the dialog shows it, and click **Delete**.

<!-- screenshot: the Delete organization dialog with the slug typed -->

If a server is offline, the organization stays, disabled, until that server is back. Then click
**Try again**.

The dashboard then opens another of your organizations. With none left, it offers **Create an
organization**.

Once it's gone, its slug is free, and a new organization can take it. Old links and emails that
name the slug open the new organization only for its members, and for Ployz support if it allows
support access. Once its **Domain** has served your services, no organization gets it again.

From the CLI, `ployz org rm ORGANIZATION` does the same. If this device was signed in to that
organization, it moves to another of yours and says which. With none left, run `ployz logout`,
then `ployz login`.

An agent's sandbox ends 24 hours after the agent made it, unless someone keeps it. Ployz then
removes it. See
[When nobody claims it](try-without-an-account.md#when-nobody-claims-it).
