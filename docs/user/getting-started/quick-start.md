---
title: Quick start
description: Deploy an example app to your own server and open it at an https address.
---

This page puts a small example app,
[render-examples/express-hello-world](https://github.com/render-examples/express-hello-world),
online at an https address on your own server.

You need a GitHub account and a server: Ubuntu LTS, Debian 12–13 or Amazon Linux 2023 with a
public IP address, that you can reach as root over SSH. A small VPS from any provider, like Hetzner or DigitalOcean, is plenty to
start. Builds run on it too, so give it 2 GB of memory or more.

## Sign in

1. Go to [ployz.dev](https://ployz.dev) and click **Deploy**.
2. GitHub opens. Approve Ployz there.

Ployz creates your organization and opens **Add your app**.

## Add the example app

1. Choose **GitHub repository**.
2. Paste `https://github.com/render-examples/express-hello-world` into the search box.
3. Under **Public repository**, click **Deploy render-examples/express-hello-world**.

![The GitHub Repository picker with the example's URL pasted](../images/service-source-github.png)

Ployz creates a project named `express-hello-world` (with a short suffix if that name is taken), and the app appears on its canvas as the
service `express-hello-world`. Nothing runs yet: your changes wait in the bottom bar until you
deploy.

**Using your own app?** Click **Connect GitHub** instead and install the Ployz app on your
repository, so every push deploys it; see [Deploy from GitHub](../deploy/github.md). Your app
must listen on the `PORT` variable, or you enter its port in the next step; see
[Choose the port](../services/domains.md#choose-the-port).

## Give it an address

1. Click the `express-hello-world` card, then **Settings**, then **Generate Domain** under
   **Networking**.
2. Leave **Port** blank. The example listens on `PORT`.
3. Click **Generate Domain**.

![The Generate Service Domain dialog](../images/generate-domain-dialog.png)

The address goes live on your first deploy.

## Add a server

While you have no server, the bottom bar shows **Add servers** where **Deploy** would be. Add
this first server from your terminal, so it joins and can take the deploy.

1. [Install the CLI](../cli/overview.md#install-the-cli) and sign in:

   ```sh
   curl -fsSL https://ployz.sh | sh
   ployz login
   ```

2. Run `ployz server add --command`. It prints a line to paste on the server and an enrollment
   token beside it, then waits.
3. Connect to your server with `ssh root@203.0.113.10` (your server's IP), paste the line and
   press Enter. Paste the token when it asks.
4. When the command finishes, the server shows on the **Servers** page. Not on Ubuntu, Debian or
   Amazon Linux? See [Start without ZFS](../servers/add-a-server.md#start-without-zfs).

The command installs Ployz and adds the server to your organization. If your provider puts a
firewall in front of it, open [these ports](../servers/add-a-server.md#open-the-firewall).

**Add servers** in the bottom bar makes a Fleet Token instead. A server that runs its install
line shows up under **Waiting** on the **Servers** page. Select it and click **Enroll** to
choose its storage and name. With one waiting server in a new organization, that sheet opens
automatically. See [Add many servers](../servers/add-many-servers.md) for the full flow.

Starting servers from a script or from cloud-init user data? Install them with a
[Fleet Token](../servers/add-a-server.md#install-with-a-fleet-token). Each one installs
Docker and Ployz, then waits until you say it can join.

## Deploy

Once the server has joined, **Deploy** appears in the bottom bar, next to **Apply 2 changes**: the new
service and its address.

1. Click **Deploy**. The deployment page opens.
2. Ployz builds the app, then starts it on your server. Click the build or the service to follow
   its logs.
3. Wait for **Deployed**. The first build takes a minute or two.

![Deployment #1 building express-hello-world, with Build and Deploy logs](../images/deployment-page-build.png)

While the first deploy runs, the service's card reads **Starting**. If the deployment fails,
its page says why; see
[Troubleshooting deployments](../troubleshooting/deployments.md).

## Open your app

The card turns **Online** and shows the address, like
`express-hello-world.acme-x7q2.ployz.app`. Open it from **Settings → Networking**.

![The canvas with express-hello-world Online and its address on its card](../images/canvas-online.png)

If the page doesn't load, see [My app isn't reachable](../troubleshooting/app-not-reachable.md).

**Next:** [deploy your own app from GitHub](../deploy/github.md). Connecting GitHub also turns on
deploy on push, and lets you turn on preview environments.
