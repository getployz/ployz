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
running while each server upgrades. Automatic upgrades never move a server from 0.3 to 0.4, so
you start this one yourself.

Until you activate, your servers work as they did on 0.3, and the **Servers** page shows each one
as **Awaiting activation**.

## 2. Activate

Pick one of these.

**From your computer.** Run `ployz device enforce` from a device you approved, as in
[Require a device's signature](manage-servers.md#require-a-devices-signature-for-restarts-and-joins).
Run it without `--connect` in the context where you added your servers over SSH, so it reaches
each one over its own SSH connection. This works for every cluster. A server that only joined
through Ployz Cloud has no SSH connection saved; confirm its key with `sudo ployz status` on it.

**From the dashboard.** When every server runs 0.4 and none holds a device, passkey or server it
can't vouch for, the dashboard offers **Activate** on the **Servers** page. Otherwise it names
what stops it. Your passkey approves the list of servers.
A second passkey prompt approves the cluster's new key for sealed values, which one of your
servers makes. If the dashboard says a server holds keys from before, activate from your
computer instead. The dashboard never shows a recovery code, so make one afterwards from your
computer, as in [Your recovery code after activation](#your-recovery-code-after-activation).

Activation:

- removes every device and passkey added while enforcement was off, unless you keep it. Approve
  a removed one again with `ployz device request` on that computer, then
  `ployz device approve <code>` from yours.
- leaves out a server you didn't confirm or that runs a Ployz older than 0.4. Once the others
  activate, that server is no longer part of the cluster: remove it with `ployz server rm`, then
  add it again with a join you approve. Upgrade every server first.
- cancels any build Ployz Cloud approved and any volume change waiting on an approval. Start them
  again from an approved device or with your passkey.
- gives the cluster a new key for sealed values. Values you seal from then on use it. A server
  you add later gets the new key when it joins.

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
  [Private Access](../services/private-networking.md) router it had, exactly as they were.
  `ployz device enforce` and the dashboard name each server that keeps a router, and the tailnet
  it joined. If
  you don't recognize that tailnet, run `ployz private-access off`, then turn it on again from
  an approved device.

## Your recovery code after activation

Values sealed after activation use the new key, so they need a recovery code of their own. Your
[recovery code](recovery-code.md) from before still opens values sealed before activation. Keep
both codes.

- **From your computer.** `ployz device enforce` prints the new **Recovery code** once, in a
  terminal, while you're signed in with `ployz login`. It says that the code opens values sealed
  after activation and that your earlier code still opens values sealed before.
- **From the dashboard, or if you got no code.** From your computer, signed in, run:

  ```sh
  ployz recovery rotate --connect ssh://root@203.0.113.10
  ```

  Give it a server that activated. It prints a code for the new key and changes nothing about
  your earlier code.

To use it, run `ployz recover` as in [When a server shows Needs recovery](recovery-code.md#when-a-server-shows-needs-recovery):
type your earlier code first to get back the old key, then run `ployz recover` again with the
new code to get back the new key.

Next: [approve a server's join](add-a-server.md#approve-a-join-when-device-enforcement-is-on).
