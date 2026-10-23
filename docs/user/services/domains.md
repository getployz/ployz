---
title: Domains
description: Give a service an https address, use your own domain, and choose the port it reaches.
---

A service is [private](private-networking.md) until it has a domain. Ployz gives each service a
free https address under your organization's domain, and you can add domains you own, also free.

## Generate a domain

1. Open your service and go to **Settings → Networking**.
2. Click **Generate domain**.
3. Leave **Port** blank to use `PORT`, or enter the port your app listens on. Click
   **Generate Domain**.
4. Click **Deploy**.

![Settings → Networking with a generated domain that uses PORT, and the private name](../images/service-settings-networking.png)

The address is the service's name under your organization's domain, like
`web.acme-x7q2.ployz.app`. It shows in full, with a copy button, as soon as you generate it, and
answers once you deploy. Generating your organization's first domain reserves its domain. A [branch](../environments/environments.md) adds its own name:
`web-staging.acme-x7q2.ployz.app`. Your organization's domain never changes; it's under
**Domain** in **Organization → General**.

To change a service's subdomain, click the pencil on its domain, edit **Subdomain**, and click
**Save domain**.

### Add another generated domain

A service can have several generated domains, like `web` and `shop`, each with its own port.
Click **Generate Domain** again, edit **Subdomain**, which suggests a free one, like `web-2`,
and click **Generate Domain**. The pencil on each domain changes only that one.

From the CLI, name the subdomain: `ployz domain add web --prefix shop`. Without `--prefix`, a
service that has a generated domain keeps it, and the CLI suggests a free subdomain. To change
one of several, name it: `ployz domain set web store --domain shop`.
A full hostname can be at most 253 characters, so under a long cluster domain Ployz refuses a
subdomain that would make it longer, when you add or change it. A branch adds its name to each
subdomain it copies, such as `web-fix`, so Ployz also refuses a branch, or a sync into one, whose
name makes a copied hostname too long, or a copied subdomain longer than 63 characters. Choose a
shorter branch name. A pull request that can't land in a branch for this reason skips that branch.
`ployz get web` or `ployz get web.domains` lists a service's domains, and `ployz diff` shows a staged one by
its full hostname, `web.domains: added shop.acme.ployz.app`.

## Choose the port

A domain sends traffic to one port inside your service. Left blank, it uses `PORT`, which is
`8080` unless you set it.

If your app listens on a fixed port, like 3000, do one of these and deploy:

- add a `PORT` [variable](variables.md) set to `3000`, or
- click the pencil on the domain, set **Target port** to `3000`, and click **Save domain**.

Either way the healthcheck follows, since it checks the port your domains send traffic to.

The domain's row shows where traffic goes: **→ Uses PORT** or **→ Port 3000**. Ployz doesn't
send traffic to the port your image exposes, but the deployment warns when none of its domains
send to one: "web's image nginx exposes 80; its domains send to 8080. Set the domain port or
PORT." The deployment still goes ahead.

## Add a custom domain

Custom domains are free.

1. In **Settings → Networking**, click **Custom domain**.
2. Enter the **Domain**, like `app.example.com`, leave **Target port** blank to use `PORT`,
   and click **Save route**.
3. Create the DNS records below at your DNS provider.
4. Click **Deploy**.

Domains ignore case and surrounding whitespace. Ployz saves ` Acme.COM ` as `acme.com`,
so you see it in lowercase everywhere.

You can enter a domain with non-Latin letters, like `пример.рф`. Ployz saves it in the
`xn--` form DNS uses, `xn--e1afmkfd.xn--p1ai`, and shows that form everywhere, DNS records
included.

`ployz domain add web app.example.com.` trims one trailing root dot, since it names the
same domain as `app.example.com`.

New custom domains must be hostnames you own. Ployz refuses IP addresses, names whose
last label is all digits, invalid punycode `xn--` labels, `ployz.app` and its subdomains,
and names in your organization's Cluster Domain zone. Use generated domains for Ployz's
reserved zones. Older stored domains remain readable and removable. You can re-add one
on the same service and environment to change its port. Names in your organization's Cluster Domain zone still require generated domains.

For an invalid hostname, `ployz domain add` names the reason and exits with code 2.
Drop any URL scheme, port or path. A hostname cannot contain underscores or wildcards.
Use at most 63 ASCII characters per label and 253 for the whole hostname after conversion
to punycode.

A service can have any number of custom domains next to its generated domains, each with its
own port.

## Point DNS at your servers

| Your domain | Type | Name | Value |
| --- | --- | --- | --- |
| A subdomain, like `app.example.com` | CNAME | `app` | your organization's domain, like `acme-x7q2.ployz.app` |
| The root, like `example.com` | A (AAAA for IPv6) | `@` | the public IP of each server that takes web traffic, one record each |

**Show DNS records** on the domain's row lists the exact records. From the CLI,
`ployz domain add web app.example.com` prints the same records, like
`Add DNS for app.example.com: CNAME app acme-x7q2.ployz.app`, and `ployz domain ls` and
`ployz domain check` print them until the domain is live. Delete any old A or AAAA
records for the same name, like your previous host's.

A CNAME follows your servers as you add and remove them. A records don't, so update them when
your servers change. If your DNS provider supports ALIAS records or CNAME flattening, point the
root at your organization's domain instead.

## Use https

Every domain serves https, and Ployz gets and renews its certificates for you. A custom
domain gets its certificate once it's deployed and its DNS points at your servers.

Keep ports 80 and 443 open on the servers that take web traffic: they use port 80 to get
certificates. Once a domain has its certificate, `http://` links redirect to `https://` with the
same path. Until then, plain http shows **Nothing is deployed here**.

## Check a domain's status

Each domain's row shows its status after the port, like **→ Uses PORT · Issuing certificate**.
A live domain shows no status.

| Status | What to do |
| --- | --- |
| **Live after your next deploy** | Click **Deploy**. |
| **Deploying**, **Setting up**, **Issuing certificate** | Nothing. It goes live on its own. |
| **Waiting for DNS**, **DNS doesn't point here yet**, **DNS points to another server** | [Fix your DNS records](../troubleshooting/app-not-reachable.md#my-custom-domain-is-waiting-for-dns). |
| **Port 80 is closed on …** (shown only when no server takes the traffic) | [Open port 80](../troubleshooting/app-not-reachable.md#port-80-is-closed) in that server's firewall, for example `ufw allow 80/tcp`. |
| **Something else answers on port 80 of …** (shown only when no server takes the traffic) | [Free port 80](../troubleshooting/app-not-reachable.md#something-else-answers-on-port-80) on that server. |
| **Ingress isn't running on …** (shown only when no server takes the traffic) | [Deploy](../troubleshooting/app-not-reachable.md#ingress-isnt-running) to bring it back. |
| **No Server receives traffic** | [Add a server](../servers/add-a-server.md). |
| **via proxy** | Nothing. It works behind a proxy like Cloudflare. |

For any other status, see [My app isn't reachable](../troubleshooting/app-not-reachable.md).

## Remove a domain

Click the trash icon on the domain. It shows **Removed on your next deploy** and keeps serving
until you deploy. To keep it, discard the change in **Details** before you deploy.

## Move a domain to another service

Remove the domain from the service that has it, add it to the other service, and deploy both
services at once. Ployz starts the new service first and moves the domain over once it's
healthy. Visitors get an error for a few seconds while the old service stops.

Ployz refuses the move in one deployment when the two services swap domains, or when the new
service depends on the old one. Move it in two deployments instead: remove the domain and
deploy, then add it to the other service and deploy again. The domain is down between the two.

## Good to know

- **No wildcards.** Ployz doesn't support domains like `*.example.com`. Add each hostname.
- **No variable holds your public address.** If your app needs it, add one, like
  `APP_URL=https://app.example.com`.
- **Domains like `app.example.co.uk`.** **Show DNS records** assumes your domain ends in one
  part, like `.com`. Here, use `app` as the name.
- **Behind Cloudflare?** Set **SSL/TLS** to **Full (strict)**. See
  [My domain is behind Cloudflare](../troubleshooting/app-not-reachable.md#my-domain-is-behind-cloudflare). Your apps
  see the visitor's address, not Cloudflare's.
