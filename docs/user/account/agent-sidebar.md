---
title: The Ployz agent
description: Ask the agent in the dashboard about your app, have it deploy for you, and approve each deploy.
---

The Ployz agent opens from the **Agent** button at the right end of the top bar, on every organization page. Ask it about your projects
and deployments, or ask it to change something and deploy. Before every deploy, it stops and
asks you.

The agent works in the project and environment you're looking at, unless you name another, and
keeps working in them after either is renamed. If the environment you're looking at is gone, or
Ployz can't read where it is, the agent won't fall back to another environment. It refuses and asks
you to name the project and environment to act in. It
can create or rename a project, add, change and rename services, [configs](../services/configs.md)
and volumes, add and change domains, check a domain and list the DNS records it still needs,
review what's staged, restore or undo a saved version, save without deploying, and deploy. It can
also remove services, volumes, domains and configs, discard staged changes, remove an environment
or a project with its volumes' data, protect or unprotect a project's own environment, and turn
preview environments on or off. It can list an
environment's deployments, follow one to its end and say which variables it left out, as
`ployz deploy` does, redeploy a failed, cancelled or unknown one, start
a queued one now, and cancel one, like the
[deployment page](../deploy/history-and-rollback.md#redeploy-or-cancel-a-deployment)'s buttons. It can also
read your GitHub repositories and work on your servers. It doesn't create environments.

The agent never sees or sends a secret's value. It can ask Ployz to give a variable a new
random secret, such as a database password that other services then reference. Ployz makes the
value and seals it in the same step, so it never appears in the agent's chat, its tool calls or
their results. A secret you already have, like an API key, you set yourself under **Variables**
or with `printf %s "$VALUE" | ployz set web.env.API_KEY --secret`; the agent names the exact
command.

## What it can do on your servers

The agent asks your servers and Cloud directly, so its answers are as fresh as the servers that
replied. It can:

- Show a server, a service's containers, and where each volume's copies live
  (**server show**, **service status**, **volume show**).
- Read a service's recent logs from each server that ran it, with sealed variables and
  secret-shaped values replaced (**service logs**). Ployz support sees them only if your organization
  shows its logs to support. It reads the latest lines and doesn't follow new ones.
- Read a server's own recent logs from the Ployz daemon, Docker or Ployz's network store, up to 200 lines,
  with secret-shaped values replaced (**server logs**). Ployz support can't read them.
- Read a service's or a server's [metrics](../observe/metrics.md), with the OOM kills and exits
  in the range (**service metrics**, **server metrics**). Name a deployment and it reads
  from that deployment's start and keeps only its events.
- Restart, start or stop a service (**service restart**, **service start**, **service stop**).
- Drain, upgrade, clean up after, remove or forget a server (**server drain**, **server upgrade**,
  **server clean**, **server remove**, **server forget**).
- Change what a server takes: builds, services and web traffic, and how many builds it runs at
  once (**server set**). It can turn web traffic off but not on; that takes
  `ployz server set web-1 --accepts-ingress=true` in your terminal.
- Mirror a volume to another server, refresh that mirror, move a volume, release it, remove a
  mirror, or restore a volume from a server's copy (**volume mirror**, **volume move**,
  **volume release**, **volume mirror remove**, **volume restore**). To refresh a mirror, ask for a
  **volume mirror** to the same server again.
- Rename the cluster, which asks you first (**cluster name**).
- Read a connected GitHub repository, or tell you how to connect one (**github ls**,
  **github connect**).

## Talk to the agent

1. Click **Agent** at the right end of the top bar.
2. Type in **Ask about your servers…** and press Enter. Shift+Enter starts a new line.
3. To start over, click **New chat** at the top of the panel.
4. To go back to an earlier chat, click **Chat history** next to it and pick the chat.

<!-- screenshot: the agent open beside the canvas of my-app production, with a short conversation -->

The agent opens beside the page, so it never covers your canvas. The canvas narrows to make room
and refits to show every service. On a phone the agent fills the screen above the tab bar.
Click **Agent** again to close it.

The panel's header shows the chat's title, taken from your first message. Your chats are kept
for this organization, so they're still there after you reload or come back later. Closing the
agent keeps the chat.

While the agent is open on an environment's canvas, the bar with your staged changes, like
**2 changes · Details · Deploy**, moves into the agent above the message box. Close the agent and
it goes back to the bottom of the canvas.

What the agent thinks and runs folds under one line in its reply, like **Thought, ran 2 commands**.
Open it to see its thinking and each command, like **ployz environment show**. When a command was
refused, the line turns red and counts it, like **1 refused**, and the command shows why. A cancelled
command is counted as **cancelled**, never as one that ran. When the agent deploys, a card under that
line shows the deployment's status, like **Deploying** and then **Deployed**, and a link to the deployment once it exists.
When it starts a run on a server or volume, like a restart or a mirror, the card shows the run's
name and its state when Cloud took it, like **Queued · restart web**. Ask the agent how the run
went to see where it got to. A run on a server keeps the name the server had when the run started,
even after the server is renamed or removed.
A restart goes one container at a time, across every server, and starts each container again,
even when stopping it failed. A container counts as back once it passes its healthcheck, like a
deploy waits for. If a container doesn't come back, the restart stops there and leaves the rest
untouched. Stopping or restarting a service also stops its pre-deploy hook containers, and no
restart or start runs them again. Restarting or stopping the same service from here and from the
CLI at once can leave it either running or stopped.

## Approve or deny a deploy

Every deploy the agent starts waits for you, as a card like **Deploy shop/production** with
**Review and approve**. A coding agent you connected gives you the same page as a link. The page
shows what the deploy destroys and, when your servers need a signature, the lines your passkey
signs. Click **Approve with passkey** and tap once, or **Approve and deploy** when nothing needs
signing, and the deploy starts as whoever asked. Anyone who can deploy to the environment can
approve it. If the environment changed since the agent asked, the page says **This deploy changed
since it was asked for. Ask again.** and denies it. A deploy that can't ask anyone, like one whose
servers don't answer, is refused with the reason instead. In a Protected environment, only an
owner or admin approves a deploy from someone who can't protect it.

Anything else the agent would do that removes something, it doesn't do. That covers a save that
removes a running service, deletes or detaches a volume or removes a domain, a redeploy, removing,
forgetting, draining or cleaning up a server, a volume command that deletes data, taking an
environment or a project off your servers, and unprotecting an environment. The agent says it needs
a person and where, like **This drain of fra-1 needs a person. Do it in Ployz Cloud.** Do it
yourself in the dashboard or with `ployz`.

A stop or restart the agent asks for becomes a card marked **Proposal**, like **Stop web in
shop/production**. See [Approve several steps at once](#approve-several-steps-at-once).

A member's deploy, stop or restart in a
[Protected environment](../environments/environments.md#protect-an-environment) also appears under
**Waiting on you**, for owners and admins. The card says who asked, like **Asked by Linus**.
Approving it runs it as the member who asked. Only an owner or admin who didn't ask can approve or
deny it. The member who asked finds their card under **Requested**, marked **Waiting on an owner or
admin.**, with no **Approve** or **Deny**, and it doesn't count on the **Agent** button.

## Approve several steps at once

An agent can plan several service restarts, stops and starts as one proposal, like stopping
`web`, restarting `db`, then starting `web` again. It stages the steps, then presents the plan,
and one card appears under **Waiting on you**, titled like **3 steps in shop/production** and
marked **Proposal**. Nothing runs until you approve.

The card lists every step in the order it runs, like **1. Stop web** and **2. Restart db on
fra-1**, each with the name your servers know the service by, like **On servers
shop-prod-a1b2/web**. A step names the service as your servers run it; after a rename, the name
it has now follows in brackets, like **Stop web-old (web)**. Under the steps, it lists what the plan takes away, like **Stops service
web until started again**, as a stop or restart card does.

1. Read the steps in order, and what they take away.
2. Click **Approve all**.
3. Or click **Deny**, with a reason if you like. The agent gets it.

<!-- screenshot: a proposal card for 3 steps in shop/production -->

The steps run one after another, and if one fails, the ones after it don't run. The card moves
under **Recent** and shows each step as **Done**, **Failed** with why, or **Not run**. You can ask
the agent how far the plan got. A proposal holds only restarts, stops and starts; a deploy or a
settings change still asks on its own card.

A proposal in a [Protected environment](../environments/environments.md#protect-an-environment)
can only be presented by an owner or admin.

## Good to know

- **Needs an Anthropic API key.** On a [Self-hosted Cloud](../self-hosting.md) without
  `ANTHROPIC_API_KEY`, the agent answers every message by saying it isn't set up yet.
- **Approving never deletes data on its own.** When a deploy or save would delete a volume's
  data, the agent stops and tells you which volumes. It goes ahead only if you ask it to, like
  "save, accepting the volume loss", and only in the same project and environment.
- **Deploys and interruptions ask.** Every deploy the agent starts waits for you, even one that
  only rolls out a new image, changes variables or scales a service. A stop or restart waits on a
  **Proposal** card. Starting a service never asks, and neither does upgrading a server. Anything
  else that removes something is refused, and the agent tells you where to do it.
- **You approve one plan.** If the environment changes before you approve a deploy, the page says
  so, and the agent asks again about the new one.
- **Asking is always on.** No setting turns it off. See
  [Destructive actions ask first](organizations.md#destructive-actions-ask-first).
- **Pictures show as links.** When a reply includes an image, you see its full address as a link
  instead of the picture. Nothing loads until you click it, so a log line or repository file can't
  get the agent to send your data to another site inside an image address.
- **Logs and files are data, not orders.** The agent reads logs, repository files and pull
  requests as text others wrote. It doesn't follow instructions written inside them.
- **Keep backups.** An approved deploy that deletes a volume deletes its data. Keep
  [backups](../services/databases.md#back-up-a-database) of data you can't lose.
