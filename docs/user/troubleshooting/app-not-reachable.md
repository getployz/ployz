---
title: My app isn't reachable
description: Fix "This app didn't respond", "Nothing is deployed here", wrong ports, custom domains, certificates and services that can't reach each other.
---

A visit to your app is a short trip: the browser looks up your domain, reaches one of your
servers over https, and that server hands the request to your app on its port. When a page
won't load, one leg of the trip is broken.

```mermaid
flowchart LR
  browser(["Browser"]) -- "DNS" --> server["One of your servers"]
  server -- "PORT" --> app["Your app"]
```

Start with your domain's status in the service's **Settings → Networking**: it names most of
the problems below. Your app's own errors are in the service's **Logs**. To see which server
answered and what it knows about your service, [trace a request](trace.md).

When Ployz can't hand a request to your app, your server answers with its own page. Its title is
a heading below, and its code (like `502 · NO_RESPONSE`) is on the page and in the `ployz-error`
response header.

## My service has no address

Services you add in the dashboard don't get a public address until you ask for one. In
**Settings → Networking**, click **Generate Domain**, leave **Port** blank to use `PORT`, then
deploy.

![The Generate Service Domain dialog](../images/generate-domain-dialog.png)

## This app didn't respond

You'll see `502 · NO_RESPONSE`, then `503 · NO_RESPONSE` for the next 30 seconds while your
server stops sending requests to a replica that didn't answer. Your server got the request, but
your app didn't answer.

1. **Listen on `0.0.0.0` and `PORT`.** An app listening on `localhost` or `127.0.0.1` turns
   away requests from outside its container:

   | App | Listen on all addresses and `PORT` |
   | --- | --- |
   | Node.js, Express | `app.listen(process.env.PORT, "0.0.0.0")` |
   | Next.js | `next start` already does. With `output: "standalone"`, add the variable `HOSTNAME=0.0.0.0`. |
   | Vite preview | `vite preview --host 0.0.0.0 --port $PORT` |
   | gunicorn | `gunicorn --bind 0.0.0.0:$PORT app:app` |
   | uvicorn | `uvicorn main:app --host 0.0.0.0 --port $PORT` |
   | Rails, Puma | `bundle exec puma -b tcp://0.0.0.0:$PORT` |
   | Laravel | `php artisan serve --host=0.0.0.0 --port=$PORT` |

   Change it in your code or in **Settings → Deploy → Start command**. If Vite preview says
   "Blocked request. This host is not allowed.", add your domain to `preview.allowedHosts` in
   `vite.config`.
2. **Check the port matches.** See the next section.
3. **Check your app is running** in its **Logs**. A replica that crashes or fails its health
   check gets no traffic; see [Troubleshooting deployments](deployments.md).
4. **Check the server running it is up.** A replica on a server that's down or out of reach
   doesn't answer either. See [Troubleshooting servers](servers.md).

## This app isn't running

You'll see `503 · NOT_RUNNING`. The service has no healthy replica, so there's nothing to send
the request to. The page reloads itself every 10 seconds.

- **A deploy is in progress.** The page goes away once the first replica passes its health
  check.
- **The service is stopped.** Deploy it, or run `ployz service start <name>`. See
  [Restart, stop or open a shell](../services/services.md#restart-stop-or-open-a-shell).
- **Every replica fails its health check.** See
  [Troubleshooting deployments](deployments.md).

## My app listens on a different port

The domain's row says where traffic goes. **→ Uses PORT** means the `PORT` variable, which is
`8080` unless you set it. Either:

- add a `PORT` variable set to your app's port, then deploy (the health check uses it too), or
- click the pencil on the domain and set **Target port** to the port your app listens on.

## Nothing is deployed here

You'll see `404 · NOT_DEPLOYED`. The domain leads to one of your servers, but no service
answers for it there.

- **Check the exact name.** `www.example.com` and `example.com` are different domains. Add each
  one you use to the service.
- **On `http://`?** Ployz redirects http to https only once the domain has its certificate.
  Until then, wait for it to be issued. Behind Cloudflare, see
  [below](#my-domain-is-behind-cloudflare).
- **On `https://`?** The domain isn't live yet. If its row says
  **Live after your next deploy**, click **Deploy**.

## My custom domain is waiting for DNS

You'll see **Waiting for DNS**, **DNS doesn't point here yet** or **DNS points to another
server**. Your servers can't get a certificate until your domain leads to them.

1. Click **Show DNS records** on the domain's row and create exactly those records at your DNS
   provider.
2. Delete any other A and AAAA records for the same name, like your old host's. A leftover
   AAAA record alone can block the certificate.
3. Give DNS time to spread. Your servers keep checking on their own.

## The browser can't make a secure connection

Your servers don't have a certificate for the domain yet. The domain shows **Issuing
certificate**, a DNS status, or **Certificate failed; retrying at …**. Fix any DNS status first
(see above). Otherwise give it a little time: your servers keep trying on their own.

## My domain is behind Cloudflare

- Set **SSL/TLS** to **Full (strict)**. **Flexible** reaches your servers over plain http, which
  redirects back to https, so the browser says "too many redirects".
- If the domain shows **Your proxy redirects to HTTPS: exempt /.well-known/acme-challenge/* from
  it**, Cloudflare's redirect is blocking your certificate. Set the DNS record to **DNS only**,
  or turn off **Always Use HTTPS** and add a redirect rule that sends http to https for every
  path except `/.well-known/acme-challenge/*`.

## Port 80 is closed

You'll see **Port 80 is closed on 203.0.113.10**. Your servers use port 80 to get certificates,
so Ployz leaves that server out of your organization's domain until it's open. When your other
servers still take the traffic, your domains stay live and only **Domain** in
**Organization → General** shows **Port 80 closed on 203.0.113.10.** An offline server is left
out without a warning here: it shows **Offline** on the Servers page instead.

Open TCP port 80 and TCP and UDP port 443 to the internet on every server that takes web
traffic, in your hosting provider's firewall (like a Hetzner Cloud Firewall or an AWS security
group) as well as the server's own. Then click **Check again** under **Domain** in
**Organization → General**.

## Something else answers on port 80

You'll see **Something else answers on port 80 of 203.0.113.10**. The server answers on port 80,
but not as Ployz: another web server holds the port, often Caddy or nginx in front of a
[self-hosted Ployz Cloud](../self-hosting.md#choose-where-it-runs). Until Ployz gets the port
back, Ployz leaves that server out of your organization's domain. When your other servers still
take the traffic, your domains stay live and only **Domain** in **Organization → General** shows
**Port 80 taken on 203.0.113.10.** See what holds it:

```sh
sudo ss -ltnp '( sport = :80 or sport = :443 )'
```

Stop that web server, or move it to another server. To keep Ployz Cloud on the same server,
publish it through a Cloudflare Tunnel instead. Once port 80 is free, the server starts its
ingress on its own within a minute. You don't need to remove and re-add the server. Then click
**Check again** under **Domain** in **Organization → General**.

## None of my servers takes web traffic

You'll see **No Server receives traffic** or **No ingress Server has a public IP**. Either every server has web traffic turned off,
or Ployz doesn't know their public addresses. [Add a server](../servers/add-a-server.md), or fix
it from the CLI (these settings aren't in the dashboard yet):

```sh
# Take web traffic on web-1 again
ployz server set web-1 --accepts-ingress=true

# Tell Ployz web-1's public address
ployz server set web-1 --public-ip 203.0.113.10
```

## One service can't reach another

You'll see errors like `ECONNREFUSED 127.0.0.1:5432` or `getaddrinfo ENOTFOUND api.internal`.

- Reference the other service's address, like `${{ postgres.PLOYZ_PRIVATE_DOMAIN }}`, never
  `localhost`. Its private name is in that service's **Settings → Networking**, under
  **Private Networking**.
- Use the port it listens on inside its container, like `5432`.
- For a web service, use `http://`, not `https://`: traffic between services stays on your
  private network.
- A name that doesn't resolve can mean the other service isn't healthy (check its **Logs**) or
  is in another environment.
- Services on different servers need UDP port 51820 open between those servers.

See [Private networking](../services/private-networking.md).

## Good to know

- **Scripts get JSON.** A request with `Accept: application/json` gets the same code as JSON,
  like `{"error":{"status":503,"code":"NOT_RUNNING",…}}`, and other clients get plain text.
  Every error page carries a request id, also in the `ployz-request-id` header. Include it when
  you report a problem.
- **A redirect fails the health check.** Point **Healthcheck path** at a page your app serves
  directly. See [Deployments](../deploy/deployments.md).
- **Private names stay inside one environment.** In a branch, reference
  `${{ api.PLOYZ_PRIVATE_DOMAIN }}` rather than typing the name.
