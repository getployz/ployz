---
title: Cloudflare Tunnel
description: Serve your apps through a Cloudflare Tunnel instead of open ports, and switch back.
---

Your servers answer web traffic on ports 80 and 443 by default. With a Cloudflare Tunnel they
answer through Cloudflare instead, and the ports stay closed. Choose it when your servers can't
open ports to the internet, when something else already holds port 80, or when you want
Cloudflare in front of every request.

Both ways work with the same services and domains. Ployz runs the tunnel on every server that
takes web traffic, so a request reaches your app whichever server Cloudflare picks.

A new organization can choose Cloudflare Tunnel before its first server, on the card that
gives the install line; see
[Choose a name and Cloudflare Tunnel first](../servers/add-a-server.md#choose-a-name-and-cloudflare-tunnel-first).

## Switch from the dashboard

Open **Organization → Network**. **Web traffic** lists the two ways your servers answer. **Public
ports** is in use until you switch.

1. Create an API token in your Cloudflare profile with **Zone Read**, **DNS Edit** and
   **Cloudflare Tunnel Edit**. Cloudflare lists them as Zone → Zone → Read, Zone → DNS → Edit and
   Account → Cloudflare Tunnel → Edit.
2. Next to **Cloudflare Tunnel**, click **Set up**, paste the token and click **Find zones**.
3. Pick the zone and click **Set up**. Use the zone itself, like `example.com`, not a
   subdomain of it. Your apps get addresses like `web-prod.example.com`, which Cloudflare's free
   certificate covers.

Ployz uses the token for this one step and doesn't keep it. A token missing a permission names
it, and nothing changes. Setup is refused while a switch runs or while someone else sets
Cloudflare up, and a switch is refused while setup runs. Wait for the one under way to finish.
A setup that stops partway puts your servers back on public ports and removes what it created;
when something is left, the message names it.

Setup adds a tunnel named `ployz-<your organization>` and one proxied `*.example.com` record to
your zone. It never touches `example.com` itself or a record you already have. Your addresses
stay where they are until you switch. A Cluster whose servers still have ports closed
switches by itself as setup ends, since nothing else would serve its apps.

Click **Review and switch**. The review lists every address that moves, like
`web-prod.acme.ployz.app → web-prod.example.com`. The old ones stop working. Click **Switch**
and follow **Step 1 of 3** to **Step 3 of 3**. Expect a short interruption. The switch redeploys
only the services on that list, as they run now, and closes the old way in only once your servers
serve every new address. A service that isn't running stays stopped. Changes you haven't deployed
stay staged, so `ployz diff` still shows them. Services you deployed separately may redeploy one at
a time. The switch stops if one of those environments is still deploying. Wait for that deployment
to finish, then switch again.

While a switch runs, each row shows what your servers use right now. A second switch the
same way joins the running one. One the other way is refused until it finishes.

If a step fails, the ports and the tunnel both stay open on your servers and the section says
what stopped it, such as an environment that didn't redeploy. Each row shows how many servers use
it. Fix the cause, then click **Review and switch** again to finish.

The switch needs every server to answer. When a server is down, the switch stops with both
open and says so, since Ployz can't see which addresses that server moves. The review and each
row say when some servers didn't answer. Bring the server back, or remove it, then switch
again. The same goes for an address under the domain the switch leaves that no deployment
put there, since Ployz can't tell whether it moves. The review names it. Deploy that
environment again, or remove the address, then switch again.

The switch also stops, before it opens or moves anything, when a generated address would land on a
custom domain you already use, such as the generated `wk` becoming `wk.example.com` while a
service holds `wk.example.com` as its own domain. The review names the address. Remove the
generated domain or the custom domain, deploy, then switch again.

Custom domains stay as they are. Add each to your tunnel's DNS in Cloudflare, as the CLI steps
below show.

### Switch back

Click **Switch back** next to **Public ports** and review the same way. Your servers publish
ports 80 and 443 again and your addresses return to your organization's domain. The tunnel and
the `*.example.com` record stay in your Cloudflare account. Delete them there when you're done
with them.

A switch moves only generated addresses. Your custom domains stay, and the review lists them.
Their DNS still points at the old way in, so update their records once the switch is done; each
one's row under its service shows the records to set. A custom domain in your zone, like
`shop.example.com`, is one of them.

`ployz cluster web-traffic cloudflare` and `ployz cluster web-traffic public` ask for the same
switch from the CLI. They wait for an owner or admin to approve it in the dashboard, as do
agents. Approved while a switch the same way already runs, they say so and start no second one.

## Open the tunnel from the CLI

1. In Cloudflare, under **Zero Trust → Networks → Tunnels**, create a tunnel and copy its token.
   Don't follow Cloudflare's install steps; Ployz runs the connector for you.
2. Add a public hostname to the tunnel for each domain, with service `http://localhost:8480`.
   Point the hostname's DNS record at the tunnel; Cloudflare proxies it.
3. Switch your servers to the tunnel, with the token on stdin:

```sh
ployz ingress use tunnel --token-stdin < token.txt
```

Each server reports its doors as it switches. The tunnel opens beside the ports on every server
first, so visitors keep getting answers, and then the ports close:

```
SERVER   DOORS
web-1    tunnel
web-2    tunnel
```

To start a new Cluster whose ports never open, add `--ingress-closed` to the first server's
`ployz server add`. It starts the web server with ports 80 and 443 closed and no door open, so
nothing reaches your apps until you run `ployz ingress use tunnel`. A server that joins
meanwhile opens no door either, and so does one that restarts. The flag does nothing for a server
that joins a Cluster.

The Cluster remembers the doors that `--ingress-closed` and `ployz ingress use` set. A server
whose `ployz` is too old to follow them can't join a Cluster that is closed or uses the tunnel,
with or without the ports: update `ployz` on it and run the join command again. If `ployz ingress use` stops partway, the Cluster
keeps the doors you asked for, the command lists the servers that didn't switch, and running it
again finishes them.

Run `ployz ingress use tunnel` without the switch to type the token at a prompt. Ployz keeps the
token sealed for your servers; nobody reads it back, and a server that joins later gets it on
its own.

The CLI seals the token on your computer, to your cluster's key. Through Ployz Cloud, it seals only
to the key [pinned on your computer](../servers/manage-servers.md#pin-your-clusters-key), so run
`ployz device pin` once first. Until you do, or when Ployz Cloud sends another key, the command
stops before it asks for the token and nothing changes. Over `--connect ssh://` it reads the key
from that server and needs no pin.

A domain behind the tunnel gets its certificate from Cloudflare, so Ployz stops ordering
certificates while every server uses the tunnel. Its status says **via tunnel**. Set
**SSL/TLS** to **Full** or **Flexible** in Cloudflare; the tunnel carries plain http to your
server.

## See which doors are open

```sh
ployz ingress
```

Each server that takes web traffic lists its doors: `public`, `tunnel`, both while a switch
runs, or **closed** when its web server isn't running. `--json` returns the same rows.

## Back to open ports

```sh
ployz ingress use public
```

The ports open on every server, and the tunnel stops. Point your DNS records at your servers
again, as [Domains](domains.md#use-your-own-domain) shows, and Ployz orders certificates for
the domains that need them. Delete the tunnel in Cloudflare when you're done with it.

## Replace the token

Run `ployz ingress use tunnel --token-stdin` again with the new token. Each server restarts its
connector on the new one.
