---
title: Upgrade to Ployz 0.4
description: Upgrade your servers to Ployz 0.4, then turn device enforcement on once, and what that can and can't change.
---

Ployz 0.4 lets your servers act only on what you approved from your own devices and passkeys,
even if someone took over Ployz Cloud. You get there in two steps: upgrade every server, then
turn device enforcement on for the cluster. Turning it on is called activation. You do it once,
and it can't be turned off.

## 1. Upgrade every server

Upgrade each server from **Servers** in the dashboard, or with `ployz server upgrade`, as in
[Upgrade Ployz on a server](manage-servers.md#upgrade-ployz-on-a-server). Your services keep
running while each server upgrades.

Until you activate, your servers work as they did on 0.3, and the **Servers** page shows each one
as **Awaiting activation**.

## 2. Activate

Pick one of these.

**From your computer.** Run `ployz device enforce` from a device you approved, as in
[Require a device's signature](manage-servers.md#require-a-devices-signature-for-restarts-and-joins).
Reach each server over SSH, or confirm each server's key with `sudo ployz status` on it. This
works for every cluster.

**From the dashboard.** When no server holds a device, passkey or server it can't vouch for, the
dashboard offers **Activate** on the **Servers** page. Your passkey approves the list of servers.
A second passkey prompt approves the cluster's new key for sealed values, which one of your
servers makes. If the dashboard says a server holds keys from before, activate from your
computer instead.

Activation:

- removes every device and passkey added while enforcement was off, unless you keep it. Approve
  a removed one again with `ployz device request` on that computer, then
  `ployz device approve <code>` from yours.
- leaves out a server you didn't confirm or that runs a Ployz older than 0.4. It stays as it
  was. Run `ployz device enforce` again once you can reach it.
- cancels any build Ployz Cloud approved and any volume change waiting on an approval. Start them
  again from an approved device or with your passkey.
- gives the cluster a new key for sealed values. Values you seal from then on use it.

Each server restarts once to activate. Your services keep running.

## What activation can't undo

Activation protects what happens after it. It can't change what already happened while
enforcement was off:

- **Sealed values set before activation.** Any server that ever held the cluster's old key can
  open them, and Ployz Cloud keeps a copy of them sealed. If you're not sure who held a server in
  your cluster's past, change those secrets where they come from, then seal them again.
- **What's already running.** A container, a build, an `exec` command or an upgrade that started
  before activation keeps running until it ends. A server stops taking new input from an `exec`
  shell when it restarts to activate, but a command already running can finish.
- **Web traffic and Private Access set up before activation.** Each server keeps the
  [Cloudflare Tunnel or public ports](../services/cloudflare-tunnel.md) and the
  [Private Access](../services/private-networking.md) router it had, exactly as they were. If you don't recognize the tailnet your router joined, turn Private Access off, then on
  again from an approved device.

## Your recovery code after activation

Your [recovery code](recovery-code.md) opens only values sealed before activation. Values sealed
after it open only on the servers that activated, and no recovery code opens them yet. A server
you add after activation can't open them yet either, so run services that need those values on
the servers that activated. Keep at least two of them running. If you lose every one, set those
values again.

Next: [approve a server's join](add-a-server.md#approve-a-join-when-device-enforcement-is-on).
