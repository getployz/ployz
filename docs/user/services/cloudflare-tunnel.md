---
title: Cloudflare Tunnel
description: Serve your apps through a Cloudflare Tunnel instead of open ports, and switch back.
---

Your servers answer web traffic on ports 80 and 443 by default. With a Cloudflare Tunnel they
answer through Cloudflare instead, and the ports stay closed. Choose it when your servers can't
open ports to the internet, when something else already holds port 80, or when you want
Cloudflare in front of every request.

Both doors work with the same services and domains. Ployz runs the tunnel on every server that
takes web traffic, so a request reaches your app whichever server Cloudflare picks.

## Open the tunnel

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

Run `ployz ingress use tunnel` without the switch to type the token at a prompt. Ployz keeps the
token sealed for your servers; nobody reads it back, and a server that joins later gets it on
its own. A token that isn't a Cloudflare tunnel token is refused before any server changes.

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
