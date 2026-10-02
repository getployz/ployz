---
title: Logs
description: Read what your services print, live, and the build and deploy logs of each deployment.
---

Ployz shows what your services print, live from your servers. Each deployment also keeps its
build output, so you can see why a build failed.

## See your logs

1. Open your environment.
2. Click **Logs** in the sidebar.

Lines from every service arrive as they're written. **All services** and **All servers** narrow
the list, and **Search loaded logs** filters it. Scroll up for older lines; **Latest** brings you
back to the end.

For one service, click it on the canvas and open its **Logs** tab.

![The Logs page, with service and server filters](../images/logs-page.png)

## See a deployment's logs

1. Open the deployment from **Deployments** in the sidebar, or from **Logs** in the bottom bar
   while it runs.
2. Click the service's tab.
3. Pick **Build** or **Deploy**.

**Build** shows the build output. A Docker image service has no build. **Deploy** shows what the
new containers printed, including the pre-deploy command. See
[Deployments](../deploy/deployments.md).

![A deployment's Build logs while it builds](../images/deployment-page-build.png)

## Good to know

- **Logs stay on your servers, for a while.** Ployz doesn't copy them anywhere else. Each
  container keeps its most recent lines (if Docker was on the server before Ployz, your Docker
  log settings decide how many), and each build keeps the end of its output. To search or keep
  logs for longer, send them from your app to a log service.
- **A deploy clears the old logs.** When a deploy replaces a container, its logs go with it, so
  an older deployment's **Deploy** tab ends up empty. A replica that failed its deploy keeps its
  logs.
- **When your servers are offline**, the page says **Your servers are offline** and keeps the
  last lines they sent.
