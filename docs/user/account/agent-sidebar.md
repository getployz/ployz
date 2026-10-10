---
title: The Ployz agent
description: Ask the agent in the dashboard about your app, have it deploy for you, and approve what it would remove.
---

The Ployz agent lives on the right edge of every organization page. Ask it about your projects,
deployments and servers, or ask it to change something and deploy. Before a deploy removes
anything, it stops and asks you.

The agent can create projects, environments, services from a GitHub repository,
[configs](../services/configs.md) and domains, and set variables. Ask it to deploy a repository, like "deploy acme/web", and it reads the repository's
files to work out how to build and start your app, then sets it up and deploys it.

## Talk to the agent

1. Click the tab on the right edge of the page.
2. Type in **Ask about your servers…** and press Enter. Shift+Enter starts a new line.
3. To start over, click **New chat** at the top of the panel.

<!-- screenshot: the agent panel open over the canvas of my-app production, with a short conversation -->

The panel's header names your organization and, on an environment's pages, the environment
(like `production`). Your conversation is kept for this organization, so it's still there
after you reload or come back later. Closing the panel keeps the conversation.

Each thing the agent runs shows as a short line under its reply, named like the CLI command it
matches. When it deploys or publishes, that line becomes a card with the deployment's status,
like **Deploying** and then **Deployed**, and a link to the deployment once it exists.

## Approve or deny a removal

When a deploy or publish would remove a running service, delete a volume, detach a volume from a
service or remove a domain, the agent stops and shows a card. The card leads with what would be
destroyed. Everything else the change does is folded under a count, like **2 other changes**;
click it to see them.

1. Read the list of what gets destroyed.
2. Click **Approve** (or press ⌘↵) to let the deploy go ahead.
3. Or click **Deny** (or press Esc) to stop it. To tell the agent why, click
   **Deny with a reason…**, type the reason, and press ⌘↵.

<!-- screenshot: an approval card for Deploy production that deletes the postgres-data volume -->

The agent picks up your answer and carries on. If you denied it, the agent gets your reason.

While approvals are waiting, the tab on the right edge shows how many. An approval you didn't
start from this chat, like one a coding agent opened from the CLI, appears at the top of the
panel under **Waiting on you**, and you answer it the same way. That includes
`ployz server rm`, `ployz server drain` and `ployz server clean`, whose cards are named for the
server, like **Remove fra-1**.

## Good to know

- **Needs an Anthropic API key.** On a [Self-hosted Cloud](../self-hosting.md) without
  `ANTHROPIC_API_KEY`, the agent answers every message by saying it isn't set up yet.
- **Approving never deletes data on its own.** When a deploy or publish would delete a volume's
  data, the agent stops and tells you which volumes. It goes ahead only if you ask it to, like
  "publish, accepting the volume loss", and only in the same project and environment.
- **Only removals ask.** Deploys that roll out a new image, change variables or scale a service
  go ahead without a card. Under such a deploy you'll see **Didn't ask. Nothing destroyed.**
- **You approve one plan.** If the change moves on before you answer, the card says the plan
  changed, and the agent asks again about the new one.
- **Turning off asking.** With **Ask before destructive actions** off in
  [Organization → General](organizations.md#ask-before-destructive-actions), the agent deploys
  new removals without stopping. A card already waiting still waits for your answer, and a card
  whose plan changed still asks again about the new one.
- **Answer anywhere.** An approval is the same in every tab and on every device. Answer it once
  and every other card for it updates. If you answer a card someone already answered, it says the
  approval was already decided elsewhere, and how.
- **Keep backups.** An approved deploy that deletes a volume deletes its data. Keep
  [backups](../services/databases.md#back-up-a-database) of data you can't lose.
