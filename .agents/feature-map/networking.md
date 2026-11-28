# Networking (NET)

How visitors and services reach each other: generated and custom domains, their certificates and status, the Ingress Proxy (Caddy) on each Server that takes web traffic, the Public Door and the Cloudflare Tunnel Door, Hosted DNS for generated domains, the private WireGuard network with its private DNS (`NAME.internal`, `NAME.NAMESPACE.CLUSTER.ployz.internal`), the container network fence around Volume holders, Cluster names and Tailscale Private Access. Verifiers start at `docs/user/services/domains.md` and `services/private-networking.md`, then `troubleshooting/app-not-reachable.md` for the refusals to provoke. Lab limits that shape most rows here: Hosted DNS is fake (`*.ployz.test`, throwaway CA), so a real certificate needs a real name (sslip.io on a Boat IPv6 host); Boat Machines are public on IPv6 only; Cloudflare and Tailscale need outside accounts. 0.3.19's CLI has no `ployz ingress`, `private-access`, `cluster name` or `device pin`.

### NET-01 Generate a domain for a Service in the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production`, click `web`, Settings → Networking → **Generate domain**, leave **Port** blank, **Generate Domain**, then **Deploy** (handles in `.agents/skills/verify-dashboard/features/canvas-and-inspector.md`). Also: MCP `domain_add` with no host.
- **Proof:** the row shows the full hostname, like `web.<org-domain>`, with a copy button before the deploy; after it `curl -sk https://web.<org-domain>/.well-known/ployz/trace` answers `status=ok`; a Branch's copy reads `web-<branch>.<org-domain>`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (fake Hosted DNS)
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: the Ingress Projection reaches 0.3.19 Servers too; check a 0.3.19 Server serves the route
- **Docs:** services/domains.md#generate-a-domain

### NET-02 Add another generated domain, or change one's prefix, from the CLI
- **Surface:** CLI
- **Reach:** `ployz domain add web --prefix shop`; `ployz domain set web store --domain shop`; `ployz get web.domains`. Also: dashboard pencil → **Subdomain** → **Save domain**; MCP `domain_update`.
- **Proof:** `ployz diff` shows `web.domains: added shop.<org-domain>`; `ployz get web.domains` lists both; a prefix that makes the hostname over 253 characters, or a Branch name that makes a copied label over 63, is refused when added
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`verify-cluster up --cloud`)
- **Compat:** CLI→Cloud: 0.3.19's `domain add` has no `--prefix`; check it still adds the first generated domain
- **Docs:** services/domains.md#add-another-generated-domain

### NET-03 Choose the port a domain reaches
- **Surface:** dashboard
- **Reach:** service Settings → Networking, pencil on the domain, **Target port** `3000`, **Save domain**, Deploy. Also: `ployz domain add web --port 3000`, `ployz domain set web <prefix> --port 3000`, or a `PORT` variable.
- **Proof:** the row reads **→ Port 3000** (or **→ Uses PORT**); the healthcheck follows the port; deploying `nginx` with the default `PORT` warns `web's image nginx exposes 80; its domains send to 8080. Set the domain port or PORT.` and still deploys
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/domains.md#choose-the-port

### NET-04 Add a custom domain in the dashboard and see its DNS records
- **Surface:** dashboard
- **Reach:** service Settings → Networking → **Custom domain**, Domain `app.example.com`, **Save route**, then **Show DNS records** on the row
- **Proof:** the dialog lists `CNAME app <org-domain>` for a subdomain and one A/AAAA per ingress Server for an apex; ` Acme.COM ` saves as `acme.com`; `пример.рф` saves and shows as `xn--e1afmkfd.xn--p1ai`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/domains.md#add-a-custom-domain

### NET-05 Ployz refuses a hostname that isn't a custom domain you own
- **Surface:** CLI
- **Reach:** `ployz domain add web 203.0.113.7`, `ployz domain add web a_b.example.com`, `ployz domain add web '*.example.com'`, `ployz domain add web shop.ployz.app`, `ployz domain add web x.<org-domain>`, `ployz domain add web app.example.com.`
- **Proof:** each refusal names its reason and exits 2; the trailing-dot form saves `app.example.com`; on success it prints `Add DNS for app.example.com: CNAME app <org-domain>`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone) for the syntax refusals; local Cloud for the Cluster Domain zone refusal
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/domains.md#add-a-custom-domain

### NET-06 See each domain's status and re-check it
- **Surface:** CLI
- **Reach:** `ployz domain ls`, `ployz domain ls web`, `ployz domain check app.example.com` (`--json` on both). Also: the dashboard row status after the port (**→ Uses PORT · Issuing certificate**) with **Check**; MCP `domain_check`.
- **Proof:** `domain ls` groups Ready, Setting up and Needs attention, and prints the DNS records to create until the domain is live; a custom domain with no DNS shows **Waiting for DNS**/`DNS doesn't point here yet`; an undeployed one shows **Live after your next deploy**; with every ingress Server's port 80 blocked (`ufw deny 80/tcp`) the row reads **Port 80 is closed on <ip>**
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone) for the CLI; local Cloud for the row
- **Drive:** verify-server; verify-dashboard SERVERS=1 for the rows
- **Compat:** CLI→Cloud: 0.3.19 has `domain ls` and `domain check`; compare their output against main Cloud
- **Docs:** services/domains.md#check-a-domains-status

### NET-07 Remove a domain; it keeps serving until the deploy
- **Surface:** dashboard
- **Reach:** trash icon on the domain's row, then Deploy (or discard it in **Details**). Also: `ployz domain rm app.example.com`; MCP `domain_remove`.
- **Proof:** the row reads **Removed on your next deploy** and `curl` still answers until the deploy; after it `/.well-known/ployz/trace` reads `status=NOT_DEPLOYED`; `domain_remove` on a deployed domain says approval will be asked
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/domains.md#remove-a-domain (`ployz domain rm` is **undocumented**, only --help)

### NET-08 Move a domain to another Service in one deploy
- **Surface:** dashboard
- **Reach:** remove the domain from `web`, add it to `web2`, deploy both at once
- **Proof:** `ployz-server`/trace `deploy=` switches to the new Service after it is healthy, with at most a few seconds of errors; a swap of two Services' domains, or a move onto a Service that depends on the old one, is refused in one deploy
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/domains.md#move-a-domain-to-another-service

### NET-09 Ployz refuses a domain another Environment still serves
- **Surface:** CLI
- **Reach:** with `app.example.com` deployed in `production`, `ployz domain add web app.example.com --env staging` then `ployz deploy --env staging`
- **Proof:** the deploy refuses naming the Environment that serves it; after removing it in production and deploying, the staging deploy takes it
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/domains.md#move-a-domain-to-another-environment

### NET-10 A signed Service keeps its domains
- **Surface:** daemon
- **Reach:** deploy `web` with a domain from an approved device, then start an unsigned container claiming the same domain (`--connect ssh://` deploy with no approved device signing, or a hand-labelled container)
- **Proof:** `curl` to the domain keeps reaching the signed replicas (trace `deploy=` stays the signed deployment); the unsigned container gets no traffic
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server upgrade: after 0.3.19→0.4 every Service needs one signed deploy before its name is protected
- **Docs:** services/domains.md#good-to-know

### NET-11 Servers get and renew https certificates
- **Surface:** daemon
- **Reach:** a custom domain whose DNS points at an ingress Server, deployed; then wait, restart `ployzd`, and partition the Server mid-order
- **Proof:** `curl -v https://<name>` shows an ACME-issued certificate; `curl -sI http://<name>/x` returns a redirect to `https://<name>/x` once issued and **Nothing is deployed here** (`404 · NOT_DEPLOYED`) before; a failed order shows **Certificate failed; retrying at …**; Machines stagger orders and share the backoff
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: a real DNS name (sslip.io on a Boat IPv6 host)
- **Drive:** verify-server; verify-acme-issuance for the order gate, rank delay and shared backoff
- **Compat:** Server↔Server: the certificate row is replicated; a mixed 0.3.19/0.4 Cluster shares it
- **Docs:** services/domains.md#use-https

### NET-12 The Ingress Proxy follows route changes and survives Caddy restarts
- **Surface:** daemon
- **Reach:** deploy a domain, then `docker restart ingress`, `docker rm -f ingress` (the daemon recreates it), and deploy again
- **Proof:** after each step `curl` to the domain answers and trace `deploy=` names the newest deployment; `docker ps` lists `ingress` and `ingress-holder` (ports on `ingress-holder`)
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-caddy-load for loads lost, refused or racing a restart
- **Compat:** Server upgrade: a 0.3.19-made Ingress Proxy keeps `local` logging until `ployz server set <s> --accepts-ingress=true`
- **Docs:** troubleshooting/trace.md#good-to-know

### NET-13 Ployz answers with its own error pages when it can't reach the app
- **Surface:** daemon
- **Reach:** a domain whose app listens on `127.0.0.1`, a stopped Service (`ployz service stop web`), and an unknown hostname pinned with `curl --resolve`
- **Proof:** `502 · NO_RESPONSE` then `503 · NO_RESPONSE` for 30 s, `503 · NOT_RUNNING`, `404 · NOT_DEPLOYED`, each in the `ployz-error` header; `curl -H 'Accept: application/json'` returns `{"error":{"status":503,"code":"NOT_RUNNING",…}}`; every page carries `ployz-request-id`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/app-not-reachable.md#this-app-didnt-respond

### NET-14 Requests fail over when a replica's Server powers off
- **Surface:** daemon
- **Reach:** a Service with 2 replicas on 2 Servers; `power off` the Server of one replica while looping `curl` and a `POST` against the other ingress
- **Proof:** new `GET`s reach the live replica within about half a second; an in-flight `GET` retries after about 14 s; an in-flight `POST` gets `504 · NO_RESPONSE`; a replica that doesn't start its response in 60 s is skipped
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`power off/on`)
- **Compat:** Server upgrade: an Ingress Proxy from an earlier release keeps the 15-minute hang until **Takes web traffic** is cycled
- **Docs:** troubleshooting/app-not-reachable.md#this-app-didnt-respond

### NET-15 Trace a request to see which Server answered
- **Surface:** daemon
- **Reach:** `curl http://<domain>/.well-known/ployz/trace`; `curl -sI https://<domain> | grep -i ployz`; `curl --resolve <domain>:80:<ip> …` per Server
- **Proof:** the page lists `server deploy replicas status ip http tls sni request_id ts` in that order, always `200`; `ployz-server` equals `server=` and `ployz-request-id` equals `request_id=`; a `POST` to the path reaches the app; while the certificate is pending `status=NOT_DEPLOYED`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/trace.md#read-the-trace-page

### NET-16 Apps see the real visitor address, never a forged one
- **Surface:** daemon
- **Reach:** from a non-Cloudflare host, `curl -H 'CF-Connecting-IP: 1.2.3.4' -H 'X-Forwarded-For: 1.2.3.4' https://<domain>/` against an app that echoes its headers
- **Proof:** the app sees `X-Forwarded-For`/`X-Real-IP` set to the curl host's address and no `CF-Connecting-IP`; the trace page shows no `via=cloudflare` line; an IPv6 visitor's own address arrives on a Server with IPv6
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone) · External: a Cloudflare-proxied name for the positive half
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/app-not-reachable.md#my-domain-is-behind-cloudflare

### NET-17 The organization's domain and its port 80 check
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings?section=general`, **Domain** (ClusterDomainSection), **Check again**
- **Proof:** **Domain** shows the Cluster Domain with **Copy domain**; with `ufw deny 80/tcp` on one of two ingress Servers it reads **Port 80 closed on <ip>.** while domains stay live; with a web server holding port 80 it reads **Port 80 taken on <ip>.**; on an IPv6-only Server it says Ployz Cloud can't reach it over IPv6
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server: the check is Cloud's own; 0.3.19 ingress Servers are checked the same way
- **Docs:** troubleshooting/app-not-reachable.md#port-80-is-closed

### NET-18 Hosted DNS points generated domains at the Servers taking web traffic
- **Surface:** daemon
- **Reach:** a local Cloud with 2 ingress Servers; turn one's web traffic off, block port 80 on another, power one off
- **Proof:** `dig` of `web.<org>.ployz.test` against the local Cloud's Hosted DNS lists only Servers that take web traffic and pass the port 80 check; a Server set `--accepts-ingress=false` drops out from the next sync (within the hour, or at once after **Check again**)
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud (fake Hosted DNS)
- **Drive:** verify-server (`verify-cluster up --cloud`)
- **Compat:** Cloud→Server: check-in reports from 0.3.19 Servers still feed the records
- **Docs:** troubleshooting/app-not-reachable.md#port-80-is-closed

### NET-19 Turn a Server's web traffic off and on in the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/servers/<serverId>`, **Web traffic** → **Takes web traffic** switch → dialog → **Stop**; switch on again
- **Proof:** the dialog lists the domains pointed at the Servers and warns when this is the last ingress; the row reads **Stopping…** then **Web traffic is off**, and `docker ps` lists neither `ingress` nor `ingress-holder`; on, it reads **Starting the web server…** then **Your domains point at <ip>**; with port 80 held it reads **Couldn't start web traffic · Port 80 is in use on <server>** and offers **Retry**
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** none
- **Docs:** servers/manage-servers.md#change-what-a-server-does

### NET-20 Turn the ingress role on and off from the CLI
- **Surface:** CLI
- **Reach:** `ployz server set web-1 --accepts-ingress=false` (asks when no other Server takes traffic; `--yes`), `ployz server set web-1 --accepts-ingress=true`
- **Proof:** `false` leaves `ingress` running; `true` with `python3 -m http.server 80` on the host prints `Web traffic stays off on web-1: port 80 or 443 is in use. Free it, then try again.`, exits 3, and turns the role back off; any other failed start prints `Server updated; the Ingress Proxy did not follow` and exits 3
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.3.19 has `--accepts-ingress`; check it against a 0.4 daemon
- **Docs:** servers/manage-servers.md#change-what-a-server-does

### NET-21 A deploy brings back a missing Ingress Proxy
- **Surface:** CLI
- **Reach:** `docker rm -f ingress ingress-holder` on an ingress Server, then `ployz deploy --plan` and `ployz deploy`
- **Proof:** `--plan` lists `ingress on web-1: would create`; after the deploy both containers run and the domain answers; a crash-looping proxy is left alone with a warning naming the `docker logs` command
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/app-not-reachable.md#ingress-isnt-running

### NET-22 Tell Ployz a Server's public IP
- **Surface:** CLI
- **Reach:** `ployz server set web-1 --public-ip 203.0.113.10`, `--public-ip 127.0.0.1`, `--public-ip 10.0.0.5`, `--public-ip none`
- **Proof:** `ployz server show web-1` shows the set address and it survives a `ployzd` restart; `127.0.0.1`, `0.0.0.0`, `::1` and `fe80::1` are refused; `10.0.0.5` warns; `none` clears it with no lookup after; with none set on every ingress Server the domain reads **No ingress Server has a public IP**
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.3.19 has `--public-ip`
- **Docs:** troubleshooting/app-not-reachable.md#none-of-my-servers-takes-web-traffic

### NET-23 See which Doors each ingress Server opens
- **Surface:** CLI
- **Reach:** `ployz ingress`, `ployz ingress --json`
- **Proof:** a `SERVER DOORS` row per ingress Server reading `public`, `tunnel`, both during a switch, or `closed` after `docker rm -f ingress`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.3.19's CLI has no `ployz ingress`
- **Docs:** services/cloudflare-tunnel.md#see-which-doors-are-open

### NET-24 Switch every ingress Server to the Cloudflare Tunnel from the CLI
- **Surface:** CLI
- **Reach:** `ployz ingress use tunnel --token-stdin < token.txt` (through Cloud, after `ployz device pin`); replace the token by running it again
- **Proof:** rows end `tunnel` on every Server; `ss -ltn` shows nothing on :80/:443; a `curl` loop to the tunnel hostname never fails during the switch; through Cloud without a pin it stops before asking for the token; a domain behind the tunnel reads **via tunnel** and no certificate is ordered; a partial switch lists the Servers that didn't switch and a rerun finishes them
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: a Cloudflare account, tunnel token and zone
- **Drive:** verify-server; verify-acme-issuance for "every Server uses the tunnel stops orders"
- **Compat:** CLI→daemon, Server↔Server: 0.3.19 has no Tunnel Door, and a Server whose `ployz` is too old can't join a closed or tunnel Cluster
- **Docs:** services/cloudflare-tunnel.md#open-the-tunnel-from-the-cli

### NET-25 Go back to open ports from the CLI
- **Surface:** CLI
- **Reach:** `ployz ingress use public`
- **Proof:** rows end `public`; `ss -ltn` shows :80 and :443 on `ingress-holder`; the connector container is gone; certificates are ordered for custom domains that need them
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: a Cloudflare tunnel to leave
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.4 only
- **Docs:** services/cloudflare-tunnel.md#back-to-open-ports

### NET-26 Start a new Cluster with its ports closed
- **Surface:** CLI
- **Reach:** `ployz server add … --ingress-closed` on the first Server; join a second; restart one
- **Proof:** `ployz ingress` reads `closed` on every Server, including the joiner and after a restart; nothing answers on :80/:443 until `ployz ingress use tunnel`; the flag does nothing on a joiner
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`verify-cluster up --bare`)
- **Compat:** Server↔Server: a Server too old to follow the Doors can't join and is told to update
- **Docs:** services/cloudflare-tunnel.md#open-the-tunnel-from-the-cli

### NET-27 Set up Cloudflare and switch web traffic from the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings?section=network`, **Web traffic** (WebTrafficSection) → **Cloudflare Tunnel** → **Set up**, paste the token, **Find zones**, pick the zone, **Set up**; **Review and switch** → **Switch**, Step 1 of 3 to 3 of 3; also CLI `ployz cluster web-traffic cloudflare`, published by Cloud (signed in)
- **Proof:** a token missing a permission names it and nothing changes; setup creates tunnel `ployz-<org>` and one proxied `*.<zone>` record; the review lists **Addresses that move** (`web-prod.<org-domain> → web-prod.<zone>`) and **Custom domains that stay**; after the switch the rows read the tunnel for every Server; a down Server or a generated address landing on a custom domain stops the switch before anything moves, named in the review
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud · External: a Cloudflare account and API token
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server: a 0.3.19 Server can't open the Tunnel Door; check the switch refuses it by name
- **Docs:** services/cloudflare-tunnel.md#switch-from-the-dashboard

### NET-28 Switch back to public ports from the dashboard
- **Surface:** dashboard
- **Reach:** Organization → Network, **Switch back** next to **Public ports**, review, **Switch**; also CLI `ployz cluster web-traffic public`, published by Cloud
- **Proof:** generated addresses return to `<org-domain>`; ports 80/443 answer again; the tunnel and `*.<zone>` record stay in Cloudflare; a switch the other way while one runs is refused
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud · External: a Cloudflare account
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** none
- **Docs:** services/cloudflare-tunnel.md#switch-back

### NET-29 Choose a name and Cloudflare Tunnel before the first Server
- **Surface:** dashboard
- **Reach:** a new Organization's **Servers** page, the install-line card (first-tunnel): pick Cloudflare Tunnel, then run the install line
- **Proof:** the first Server joins with ports closed and web traffic switches to the tunnel by itself; `ployz ingress` reads `tunnel`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: a Cloudflare account and API token
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** install: a 0.3.19 install line can't join a tunnel Cluster
- **Docs:** servers/add-a-server.md#choose-a-name-and-cloudflare-tunnel-first

### NET-30 A custom domain behind the Cloudflare Tunnel
- **Surface:** dashboard
- **Reach:** with the tunnel in use, add `shop.<zone>` and `shop.example.org` as custom domains, **Show DNS records**, **Check**
- **Proof:** **Show DNS records** lists `CNAME shop <id>.cfargotunnel.com`; a name not reaching the tunnel reads **Not reaching your tunnel** or **No answer over HTTPS**; `a.shop.<zone>` reads **The free certificate covers one level under <zone>**; the custom domain `shop.<zone>` refuses the generated prefix `shop` and the other way round
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: a Cloudflare account and zone
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/domains.md#behind-a-cloudflare-tunnel

### NET-31 Services find each other at NAME.internal
- **Surface:** daemon
- **Reach:** deploy `web` and `api` (with a healthcheck); `ployz exec web -- getent hosts api.internal`; restart `ployzd` on that Server and repeat; scale `api` to 3
- **Proof:** the answer lists only ready replicas' IPv4 addresses, in rotating order; it keeps answering during the `ployzd` restart and while partitioned; nothing resolves while no replica is ready; another Environment's `api` never answers; `${{ api.PLOYZ_PRIVATE_DOMAIN }}` resolves to `api.internal`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`partition on/off`)
- **Compat:** Server upgrade: private DNS keeps answering over UDP while the daemon upgrades
- **Docs:** services/private-networking.md#how-it-works

### NET-32 Services on different Servers reach each other over WireGuard
- **Surface:** daemon
- **Reach:** `api` on machine-1, `web` on machine-2; `ployz exec web -- curl -s http://api.internal:$PORT/`; restart `ployzd` on both; block UDP 51820 between them
- **Proof:** the curl answers, and keeps answering through the `ployzd` restarts under the ack probe; `sudo wg show ployz-wg` lists the peer with a recent handshake; with UDP 51820 blocked it stops
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server (`probe`, `partition on/off`)
- **Compat:** Server↔Server: a mixed 0.3.19/0.4 Cluster mid-upgrade keeps the mesh; a 0.3.19 Server can't join a 0.4 Cluster
- **Docs:** services/private-networking.md#how-it-works

### NET-33 A container keeps its private address across Docker and Server restarts
- **Surface:** daemon
- **Reach:** note `ployz exec web -- hostname -i`; `systemctl restart docker`; reboot; `docker stop`/`start` a member by hand
- **Proof:** each container returns with the same IPv4; a new container takes an address no container used recently; an operator-stopped container stays stopped after the daemon restarts
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server; verify-bridge-notes for the Stopped and Pins notes across crashes
- **Compat:** Server upgrade: the first upgrade to 0.3.1 readdressed containers; check 0.3.19→0.4 keeps addresses
- **Docs:** services/private-networking.md#how-it-works

### NET-34 Containers and the Ingress Proxy get IPv6
- **Surface:** daemon
- **Reach:** on a Boat Machine (IPv6 public), `curl -6 https://<domain>/` against an echo app; `ployz exec web -- curl -6 https://ifconfig.co`
- **Proof:** the app sees the visitor's IPv6 in `X-Forwarded-For`; the Service reaches the internet over IPv6; a Service publishing a host port stays IPv4-only and sees the gateway, like `10.210.0.1`
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/private-networking.md#how-it-works

### NET-35 Only signed containers reach a Service that holds a Volume
- **Surface:** daemon
- **Reach:** deploy `db` with a Volume from an approved device; deploy `free` with no sealed value and no Volume over `--connect ssh://` unsigned; `ployz exec free -- nc -zv db.internal 5432` and by address; repeat from a signed Service on another Server
- **Proof:** the unsigned container's connection is refused, by name and by address; the signed one connects; a Service with no Volume takes connections from both; the 33rd signed Volume-holder container on one Server fails with `every signed data address on this Server's ployz network is taken`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server upgrade: until `ployz deploy --recreate` runs in every Environment, pre-upgrade containers keep their old reach; check the downgrade path with the fence present
- **Docs:** services/private-networking.md#who-can-reach-a-service-that-holds-a-volume

### NET-36 ping and raw sockets don't work inside a Service
- **Surface:** daemon
- **Reach:** `ployz exec web -- ping -c1 api.internal`; a healthcheck `ping -c1 db`
- **Proof:** `ping` fails with an operation-not-permitted error (NET_RAW dropped) unless the Service runs privileged; the ping healthcheck never passes; a Service deployed before the upgrade keeps `ping` until its next deploy
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server upgrade: pre-upgrade containers keep NET_RAW until redeployed
- **Docs:** services/private-networking.md#good-to-know

### NET-37 A private name that ran signed stays signed
- **Surface:** daemon
- **Reach:** deploy `api` signed; delete it; start an unsigned container under the name `api` (a `--connect ssh://` deploy no device signs)
- **Proof:** `getent hosts api.internal` from another Service returns no address for the unsigned container, and the Server refuses to create it; a signed redeploy answers again
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server upgrade: after upgrading from 0.4.0-beta.2 or earlier (and 0.3.19), names aren't kept signed until each Environment deploys once
- **Docs:** services/private-networking.md#good-to-know

### NET-38 Change a Service's private name
- **Surface:** dashboard
- **Reach:** service Settings → Networking, textbox **Private address**, type `db2`, Enter, Deploy; clear it to go back
- **Proof:** `ployz exec web -- getent hosts db2.internal` answers and the old name doesn't; a variable referencing `${{ db.PLOYZ_PRIVATE_DOMAIN }}` now holds `db2.internal`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/private-networking.md#change-a-services-private-name

### NET-39 Name or rename a Standalone Cluster
- **Surface:** CLI
- **Reach:** `ployz server add … --cluster-name acme` on the first Server; `ployz cluster name`; `ployz cluster name shop2`
- **Proof:** `ployz cluster name` prints `acme`; `ployz exec web -- getent hosts postgres.shop-production.acme.ployz.internal` answers like `postgres.internal`; after the rename the new name resolves within 30 s and the old one stops; `ployz cluster name Bad_Name` is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.3.19's CLI has no `cluster name`
- **Docs:** services/private-networking.md#name-your-cluster

### NET-40 Rename the Cluster through Ployz Cloud
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings?section=network`, **Cluster name** → **Edit** → **Save** (owner or admin); also `ployz cluster name acme2` signed in, then `--approval <DIGEST>`
- **Proof:** the row reads **Pending** until a Server is online, then the new name; the CLI prints the addresses the Servers answer now and asks yes or no before renaming; an agent's rename is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: a Cluster added through Cloud is named after the org slug; check 0.3.19 Servers take the rename
- **Docs:** services/private-networking.md#name-your-cluster

### NET-41 Reach the private network from Tailscale with the CLI
- **Surface:** CLI
- **Reach:** `ployz device pin --connect ssh://<user>@<ip>` once, then `ployz private-access on --auth-key-stdin < auth-key.txt`; `ployz private-access` (`--json`)
- **Proof:** `ployz private-access` lists every Server's router; after approving the route, a tailnet device's `nc -zv <private-ip> <port>` connects; a refused key names the Server; from a device that isn't approved the command is refused; through Cloud without a pin it stops before asking
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: a Tailscale tailnet and reusable auth key tagged `tag:ployz`
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.3.19's CLI has no `private-access`
- **Docs:** services/private-networking.md#reach-the-private-network-from-tailscale

### NET-42 Connect Tailscale from the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/integrations`, **Tailscale** → **Connect**, paste the key, passkey; **Try another key**; **Turn off**; also CLI `ployz cluster private-access`, published by Cloud
- **Proof:** the card lists each Server's router; a refused key marks that Server **Key refused**; a Server that didn't answer shows **No router** and the card reads **Partly connected**; **Approve routes** links to the Tailscale admin console
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud · External: a Tailscale tailnet
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server: a 0.3.19 Server has no router to start; check the card says so
- **Docs:** services/private-networking.md#reach-the-private-network-from-tailscale

### NET-43 Turn Private Access off
- **Surface:** CLI
- **Reach:** `ployz private-access off` (also **Turn off** in Integrations)
- **Proof:** `docker ps` on each Server lists no router; `ployz private-access` lists none; a Server that couldn't remove its router is named and the command exits non-zero, and a rerun finishes it; a later `on` registers new routers
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: a Tailscale tailnet
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.4 only
- **Docs:** services/private-networking.md#reach-the-private-network-from-tailscale
