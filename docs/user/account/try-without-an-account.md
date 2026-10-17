---
title: Try Ployz without an account
description: An agent gets its own organization with one call, deploys to your server, and hands you a link to keep it.
---

An agent can start on Ployz before anyone signs in. One call gives it a token and an
organization of its own, named Agent sandbox. It can add a server and deploy right away. To keep
what it built, a person opens the claim link and signs in with GitHub within 24 hours.

## Get a sandbox

1. Ask Ployz for a sandbox.

   ```sh
   curl -fsS -X POST https://ployz.dev/api/agent/signup
   ```

   The reply holds a `token`, the `organization`, a `claimUrl` and `expiresAt`, 24 hours from now.
2. Give the token to `ployz`. Every command after this acts in the sandbox.

   ```sh
   export PLOYZ_TOKEN=ployz_...
   ```

One address gets 5 sandboxes an hour and 20 a day. Past that, the call answers `429`. Try again
later.

## Add a server and deploy

1. On the server, as root, set the token, install Ployz and add the server.

   ```sh
   export PLOYZ_TOKEN=ployz_...
   curl -fsSL https://ployz.sh | sh
   ployz server add
   ```

2. Make a project with one service, and give it an https address on the port the image listens on.

   ```sh
   ployz project new demo
   ployz service add web --image nginx:1.29-alpine
   ployz domain add web --port 80
   ```

3. Deploy.

   ```sh
   ployz deploy
   ```

4. Run `ployz get web.domains` for the address, like `web.anon-56br6lsh.ployz.app`.

Steps 2 to 4 work from any computer with `PLOYZ_TOKEN` set. A deploy doesn't wait for an
approval while only the agent owns the organization.

## Hand over the claim link

Send the person the `claimUrl`. They open it and sign in with GitHub, and they keep the sandbox
on their way back. A person who is already signed in clicks **Keep this Organization** instead.
The organization, its servers and its projects are theirs now, and they become its owner. The
agent's token keeps working for 90 more days.

If the link says **This sandbox expired**, the 24 hours are up. Run the agent again for a new
sandbox. If someone already kept it, its owner can invite you.

## What waits for a claim

Until someone keeps the sandbox, these wait for a claim.

- add a custom domain
- create a token
- invite a member
- manage billing
- connect GitHub

Each one stops with **Claim this Organization** and the claim link. Open it, sign in, and try
again. Generated addresses work before a claim. Once a person owns the organization, an agent's
deploy asks them first, as in any organization. See [Approve a deploy](connect-an-agent.md#approve-a-deploy).

## When nobody claims it

A sandbox nobody keeps ends 24 hours after the call, and its claim link stops working. Ployz
looks for ended sandboxes every 15 minutes. When it finds one, it removes the projects, then the
organization, and the servers leave it. A claim made in time keeps everything, even when a removal
starts at the same moment.

A server that doesn't answer holds the removal up. Ployz forgets it only after an hour of silence,
then removes the organization without it. The hour starts when Ployz first fails to reach the
server. It starts again each time the server answers, and when the server is paired again. A
server answers as soon as Ployz connects to it, even if the removal then fails there. A server
that keeps answering is never forgotten, and the removal waits for it.

A forgotten server keeps running its services, and its volume data stays on the disk. To clean
it up, see [Forget a server that is gone](../servers/manage-servers.md#forget-a-server-that-is-gone).

**Next:** [connect an agent to Ployz Cloud](connect-an-agent.md) once you have an account.
