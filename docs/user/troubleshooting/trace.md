---
title: Trace a request
description: See which server answered a request to your domain, and what it knows about the service there.
---

Open `/.well-known/ployz/trace` on any of your domains to see which server answered. Your
server answers this page itself, so it works even when your app is down. The page is public. It
never shows service names, private addresses, timings or your Ployz version.

## Read the trace page

```sh
curl http://example.com/.well-known/ployz/trace
```

```text
server=3f9c0d6e1a2b4c5d8e7f6a5b4c3d2e1f
deploy=8cbbfbab-9039-474d-9546-9f71ac29f367
replicas=3f9c0d6e1a2b4c5d8e7f6a5b4c3d2e1f:2,9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d:1
status=ok
ip=203.0.113.25
http=HTTP/1.1
tls=
sni=
request_id=94736c86-4946-438e-86bb-68233e43f034
ts=1791862934
```

Each line is `key=value`, always in this order. An empty value means there is nothing to
report.

| Key | What it shows |
| --- | --- |
| `server` | The ID of the server that answered. It stays the same when you rename the server. |
| `deploy` | The deployment of the newest replica of the service on this domain. It is empty when Ployz can't tell. It can be newer than the replicas taking traffic, for example while a deploy rolls out. |
| `replicas` | How many replicas of the service each server holds, as `server:count` pairs. Stopped replicas count too, so this shows where the service is placed, not how many are healthy. |
| `status` | `ok`, `NOT_RUNNING` or `NOT_DEPLOYED`. See below. |
| `ip` | The address your server sees the request come from. Behind a proxy, this is the proxy. |
| `http` | The HTTP version of the request, like `HTTP/2.0`. |
| `tls` | The TLS version, like `tls1.3`. Empty on `http://`. |
| `sni` | The domain the browser asked a certificate for. Empty on `http://`. |
| `request_id` | The request's ID. It matches the `ployz-request-id` header. |
| `ts` | When your server answered, in Unix seconds. |

`status` says what the server would do with a normal request to this domain:

- **`ok`**: the server has a replica to send it to. Your app can still fail to answer.
- **`NOT_RUNNING`**: the service is on this domain, but no replica can take traffic. A normal
  request gets [This app isn't running](app-not-reachable.md#this-app-isnt-running).
- **`NOT_DEPLOYED`**: no service answers for this domain on this server. `deploy` and
  `replicas` are empty. A normal request gets
  [Nothing is deployed here](app-not-reachable.md#nothing-is-deployed-here).

The trace page always answers `200`, whatever the status.

## Find which server answered

Every response from your domain carries two headers:

- **`ployz-server`**: the same ID as `server` on the trace page.
- **`ployz-request-id`**: the same ID as `request_id`.

They are on your app's responses, on redirects and on Ployz's error pages. Your app can't
change them. Send the request ID along when you report a problem.

```sh
curl -sI https://example.com | grep -i ployz
```

## When a domain waits for its certificate

A domain that only uses `https://` can't show the trace page until its certificate arrives. The
secure connection fails before any page can load. Use `http://` instead.

While the certificate is being issued, the `http://` trace page shows `status=NOT_DEPLOYED`,
even though a service has the domain. Once the certificate arrives, it shows the service's real
status. See
[The browser can't make a secure connection](app-not-reachable.md#the-browser-cant-make-a-secure-connection).

## Good to know

- **Cloudflare adds three lines.** When the request reaches your server through Cloudflare's
  proxy, the page ends with `via=cloudflare`, `cf_ray` (the Cloudflare request ID) and
  `client_ip` (the visitor's address as Cloudflare reports it). A value that could break the page,
  such as one with spaces, stays empty.
  A Cloudflare Worker can change `client_ip`, so don't treat it as proof of who visited.
- **A tunnel hides Cloudflare.** Through a Cloudflare Tunnel or another proxy, the request looks
  local, so the Cloudflare lines don't appear. `ip` shows that proxy's address.
- **Only `GET` shows the trace.** Other requests to the same path go to your app.
- **Each server answers for itself.** Another server can show different `deploy` or `replicas`
  for a few seconds after a change. To ask one server directly, pin the domain to its address:
  `curl --resolve example.com:80:203.0.113.10 http://example.com/.well-known/ployz/trace`.
