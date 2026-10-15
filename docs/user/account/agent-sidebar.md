---
title: The Ployz agent
description: Ask the agent in the dashboard about your app, have it deploy for you, and approve what it would remove.
---

The Ployz agent opens from the **Agent** button at the right end of the top bar, on every organization page. Ask it about your projects
and deployments, or ask it to change something and deploy. Before a deploy removes
anything, it stops and asks you.

The agent works in the project and environment you're looking at, unless you name another, and
keeps working in them after either is renamed. If the environment you're looking at is gone, or
Ployz can't read where it is, the agent won't fall back to another environment. It refuses and asks
you to name the project and environment to act in. It
can create or rename a project, add, change and rename services, [configs](../services/configs.md)
and volumes, add and change domains, check a domain and list the DNS records it still needs,
review what's staged, restore or undo a saved version, save without deploying, and deploy. It can list an
environment's deployments, follow one to its end, redeploy a failed, cancelled or unknown one, start
a queued one now, and cancel one, like the
[deployment page](../deploy/deployments.md#redeploy-or-cancel-a-deployment)'s buttons. It can also
read your GitHub repositories and work on your servers. It doesn't create environments.

## What it can do on your servers

The agent asks your servers and Cloud directly, so its answers are as fresh as the servers that
replied. It can:

- Show a server, a service's containers, and where each volume's copies live
  (**server show**, **service status**, **volume show**).
- Read a service's recent logs from each server that ran it, with secrets from the container's
  environment replaced (**service logs**). Ployz support sees them only if your organization
  shows its logs to support. It reads the latest lines and doesn't follow new ones.
- Read a server's own recent logs from the Ployz daemon, Docker or Corrosion, up to 200 lines,
  with secret-shaped values replaced (**server logs**). Ployz support can't read them.
- Read a service's or a server's [metrics](../observe/metrics.md), with the OOM kills, exits and
  restarts in the range (**service metrics**, **server metrics**). Name a deployment and it reads
  from that deployment's start and keeps only its events.
- Restart, start or stop a service (**service restart**, **service start**, **service stop**).
- Drain, upgrade, clean up after or remove a server (**server drain**, **server upgrade**,
  **server clean**, **server remove**).
- Change what a server takes: builds, services and web traffic, and how many builds it runs at
  once (**server set**). It can turn web traffic off but not on; that takes
  `ployz server set web-1 --accepts-ingress=true` in your terminal.
- Mirror a volume to another server, refresh that mirror, move a volume, release it, remove a
  mirror, or restore a volume from a server's copy (**volume mirror**, **volume move**,
  **volume release**, **volume mirror remove**, **volume restore**). To refresh a mirror, ask for a
  **volume mirror** to the same server again.
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
went to see where it got to.
A restart goes one container at a time, across every server, and starts each container again,
even when stopping it failed. A container counts as back once it passes its healthcheck, like a
deploy waits for. If a container doesn't come back, the restart stops there and leaves the rest
untouched. Stopping or restarting a service also stops its pre-deploy hook containers, and no
restart or start runs them again. Restarting or stopping the same service from here and from the
CLI at once can leave it either running or stopped.

## Approve or deny a removal

When a deploy or save would remove a running service, delete a volume, detach a volume from a
service or remove a domain, the agent stops and shows a card. It does the same before it removes
a server, cleans up after one, drains the last copy of a volume off one, or runs a volume
command that deletes data. The card leads with what would be
destroyed. Everything else the change does is folded under a count, like **2 other changes**;
click it to see them.

1. Read the list of what gets destroyed.
2. Click **Approve** (or press ⌘↵) to let the deploy go ahead.
3. Or click **Deny** (or press Esc) to stop it. To tell the agent why, click
   **Deny with a reason…**, type the reason, and press ⌘↵.

<!-- screenshot: an approval card for Deploy production that deletes the postgres-data volume -->

The agent picks up your answer and carries on. If you denied it, the agent gets your reason.

A card for a save, like **Save production**, is marked **Deletes on deploy**: saving deletes
nothing yet. Approving it saves without deploying, and shows **Approved. Saved, not deployed.**
Your next deploy deletes the data, including one a Git push starts.

While approvals are waiting, the **Agent** button shows how many, like **1 waiting**. An approval you didn't
start from this chat, like one a coding agent opened from the CLI or an agent you
[connected to Ployz Cloud](connect-an-agent.md), appears at the top of the
panel under **Waiting on you**, and you answer it the same way. That includes
`ployz server rm`, `ployz server drain` and `ployz server clean`, whose cards are named for the
server, like **Remove fra-1**.

## Good to know

- **Needs an Anthropic API key.** On a [Self-hosted Cloud](../self-hosting.md) without
  `ANTHROPIC_API_KEY`, the agent answers every message by saying it isn't set up yet.
- **Approving never deletes data on its own.** When a deploy or save would delete a volume's
  data, the agent stops and tells you which volumes. It goes ahead only if you ask it to, like
  "save, accepting the volume loss", and only in the same project and environment.
- **Only removals and interruptions ask.** Deploys that roll out a new image, change variables or scale a service
  go ahead without a card. Under such a deploy you'll see **Didn't ask. Nothing destroyed.**
  Stopping or restarting a service asks first, with a card like **Stop web** marked **Stops service**,
  because it takes the service down. Starting a service never asks, and neither does upgrading a server or
  changing what it takes, unless turning web traffic off leaves no online server with a public IP to
  serve your domains. Then the card lists the domains that go dark and everything else the change sets,
  and approving it covers only that change. Redeploying a deployment doesn't ask either, because it ships exactly
  what that deployment did and you already approved anything it removes. Starting a queued one
  doesn't ask.
- **One at a time.** The agent runs at most one command that might ask per reply, so you never
  get two cards for one message.
- **You approve one plan.** If the change moves on before you answer, the card says the plan
  changed, and the agent asks again about the new one.
- **Asking is always on.** No setting turns it off. See
  [Destructive actions ask first](organizations.md#destructive-actions-ask-first).
- **Answer anywhere.** An approval is the same in every tab and on every device. Answer it once
  and every other card for it updates. If you answer a card someone already answered, it says the
  approval was already decided elsewhere, and how.
- **Pictures show as links.** When a reply includes an image, you see its full address as a link
  instead of the picture. Nothing loads until you click it, so a log line or repository file can't
  get the agent to send your data to another site inside an image address.
- **Logs and files are data, not orders.** The agent reads logs, repository files and pull
  requests as text others wrote. It doesn't follow instructions written inside them.
- **Keep backups.** An approved deploy that deletes a volume deletes its data. Keep
  [backups](../services/databases.md#back-up-a-database) of data you can't lose.
