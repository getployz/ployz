---
title: Connect an agent to Ployz Cloud
description: Give Claude, Codex or another MCP app the Ployz tools with a link, no CLI or token to install.
---

An agent app that speaks MCP can use the Ployz tools straight from Ployz Cloud. You add one link,
sign in once in your browser, and pick the organization the agent works in. Nothing to install on
your computer, and no token to copy.

This works on [ployz.dev](https://ployz.dev). A [Self-hosted Cloud](../self-hosting.md) doesn't
serve it; use [`ployz mcp`](../cli/overview.md#give-the-agent-ployz-tools) there.

## Connect

1. Add `https://ployz.dev/mcp` to your agent as a remote MCP server. In Claude Code:

   ```sh
   claude mcp add --transport http ployz https://ployz.dev/mcp
   ```

   In other apps, look for **Add custom connector** or **Remote MCP server** and paste the link.
2. The app opens Ployz in your browser. Sign in if you aren't already.
3. Pick the **Organization** the agent works in and click **Allow**.

<!-- screenshot: the Connect an agent to Ployz page with the Organization picker -->

The agent now acts as you in that organization. To use another organization, connect again and
pick it.

## What it can do

The same tools as the [Ployz agent](agent-sidebar.md) in the dashboard. It can create projects,
change services, configs, volumes and domains, save and deploy, follow and redeploy
deployments, and work on your servers and volumes. Tell it which project and environment to
work in.

If a reply gets lost, the agent can resend a write without running it twice, as long as it sends
the same `request_key` (or `x-ployz-request-key` header) both times. A call without one is a new
run each time, and Ployz tells agents this when they connect.

## Approve a deploy

Every deploy the agent asks for waits for you, even one that destroys nothing. A save waits
only when it would remove something. See
[Destructive actions ask first](organizations.md#destructive-actions-ask-first). The call fails
with `approval_required`, anything it would remove, and a link to your organization.

1. Open the link. The approval waits at the top of the [Ployz agent](agent-sidebar.md) under
   **Waiting on you**.
2. Read what the deploy does, and click **Approve** or **Deny**.
3. Tell your agent to try again. It sends the call with the approval, and the deploy goes ahead.

Until you answer, trying again only asks again, and nothing changes. A denied approval stays
denied. Only you approve, in the dashboard. The agent can't approve its own request, and
`ployz org approval approve` refuses it too.

## Disconnect

Log out of Ployz Cloud in the browser you connected from, and every agent you connected there
stops working. Leaving the organization does the same for that organization. The next time the
agent needs Ployz, it asks you to connect again.
