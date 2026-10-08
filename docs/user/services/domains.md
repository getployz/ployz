---
title: Domains
description: Give a service an https address, use your own domain, and choose the port it reaches.
---

A service is [private](private-networking.md) until it has a domain. Ployz gives each service a
free https address under your organization's domain, and on Pro you can add domains you own.

## Generate a domain

1. Open your service and go to **Settings → Networking**.
2. Under **Public Networking**, click **Generate Domain**.
3. Leave **Port** blank to use `PORT`, or enter the port your app listens on. Click
   **Generate Domain**.
4. Click **Deploy**.

![Settings → Networking with a generated domain that uses PORT, and the private name](../images/service-settings-networking.png)

The address is the service's name under your organization's domain, like
`web.acme-x7q2.ployz.app`. A [branch](../environments/environments.md) adds its own name:
`web-staging.acme-x7q2.ployz.app`. Your organization's domain never changes; it's under
**Domain** in **Organization → General**.

To change a service's subdomain, click the pencil on its domain, edit **Subdomain**, and click
**Save domain**.

## Choose the port

A domain sends traffic to one port inside your service. Left blank, it uses `PORT`, which is
`8080` unless you set it.

If your app listens on a fixed port, like 3000, do one of these and deploy:

- add a `PORT` [variable](variables.md) set to `3000`, which the health check follows too, or
- click the pencil on the domain, set **Target port** to `3000`, and click **Save domain**.

The domain's row shows where traffic goes: **→ Uses PORT** or **→ Port 3000**. The dashboard
ignores `EXPOSE` in your Dockerfile.

## Add a custom domain

Custom domains are free.

1. In **Settings → Networking**, click **Custom Domain**.
2. Enter the **Domain**, like `app.example.com`, leave **Target port** blank to use `PORT`,
   and click **Save route**.
3. Create the DNS records below at your DNS provider.
4. Click **Deploy**.

A service can have any number of custom domains next to its one generated domain, each with its
own port.

## Point DNS at your servers

| Your domain | Type | Name | Value |
| --- | --- | --- | --- |
| A subdomain, like `app.example.com` | CNAME | `app` | your organization's domain, like `acme-x7q2.ployz.app` |
| The root, like `example.com` | A (AAAA for IPv6) | `@` | the public IP of each server that takes web traffic, one record each |

**Show DNS records** on the domain's row lists the exact records. Delete any old A or AAAA
records for the same name, like your previous host's.

A CNAME follows your servers as you add and remove them. A records don't, so update them when
your servers change. If your DNS provider supports ALIAS records or CNAME flattening, point the
root at your organization's domain instead.

## Use https

Every domain serves https, and Ployz gets and renews its certificates for you. A custom
domain gets its certificate once it's deployed and its DNS points at your servers.

Keep ports 80 and 443 open on the servers that take web traffic: they use port 80 to get
certificates. Ployz doesn't redirect `http://` to `https://` yet, and plain http shows
**Not Found**, so share `https://` links.

## Check a domain's status

Each domain's row shows its status after the port, like **→ Uses PORT · Issuing certificate**.
A live domain shows no status.

| Status | What to do |
| --- | --- |
| **Live after your next deploy** | Click **Deploy**. |
| **Deploying**, **Setting up**, **Issuing certificate** | Nothing. It goes live on its own. |
| **Waiting for DNS**, **DNS doesn't point here yet**, **DNS points to another server** | [Fix your DNS records](../troubleshooting/app-not-reachable.md#my-custom-domain-is-waiting-for-dns). |
| **Port 80 is closed on …** | [Open port 80](../troubleshooting/app-not-reachable.md#port-80-is-closed) on that server. |
| **No Server receives traffic** | [Add a server](../servers/add-a-server.md). |
| **via proxy** | Nothing. It works behind a proxy like Cloudflare. |

For any other status, see [My app isn't reachable](../troubleshooting/app-not-reachable.md).

## Remove a domain

Click the trash icon on the domain. It shows **Removed on your next deploy** and keeps serving
until you deploy. **Undo** keeps it.

## Good to know

- **No wildcards.** Ployz doesn't support domains like `*.example.com`. Add each hostname.
- **No variable holds your public address.** If your app needs it, add one, like
  `APP_URL=https://app.example.com`.
- **Domains like `app.example.co.uk`.** **Show DNS records** assumes your domain ends in one
  part, like `.com`. Here, use `app` as the name.
- **Behind Cloudflare?** Set **SSL/TLS** to **Full (strict)**. See
  [My domain is behind Cloudflare](../troubleshooting/app-not-reachable.md#my-domain-is-behind-cloudflare).
