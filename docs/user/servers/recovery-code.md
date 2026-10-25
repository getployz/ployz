---
title: Recovery code
description: The code that brings your sealed values back after you lose every server, and how to recover with it.
---

Your [sealed values](../services/variables.md#seal-a-secret) open only on your servers, with a
key your servers keep. The recovery code is the spare key: if you lose every server, it lets
the next one open your sealed values again. Ployz Cloud keeps a backup of the key that only
this code opens, so Ployz can't open it for you.

## Get your recovery code

When you add the first server of your organization with `ployz server add` in a terminal, it
prints a **Recovery code** once, then "Keep the recovery code safe; ployz recover needs it."
Keep it in your password manager, away from your servers. Adding more servers prints no new
code.

You don't get a code when the first server joins from the dashboard's install line, from
cloud-init or a script, or with `--json` unless you add `--show-recovery-code`. Make one now,
while a server still runs. From your computer, signed in with `ployz login`:

```sh
ployz recovery rotate --connect ssh://root@203.0.113.10
```

Give it a server that runs your services, not one marked **Needs access** or **Needs recovery**.
It prints a new code, and the backup in Ployz Cloud opens only with that code from then on. Run
it in a terminal: with no terminal to show the code, it refuses and changes nothing. Only an
owner or admin can rotate the code.

Rotate the code too if you lose it or think someone else has seen it. You need a running
server to do that.

## When a server shows Needs recovery

After you [forget every server](manage-servers.md#forget-servers-you-deleted) and add one again,
the new server doesn't have the key yet. Adding it says "This Server needs access to the
Sealing Key; ployz recover restores it.", the **Servers** page shows it as **Needs recovery**,
and a deploy that needs a sealed value fails with "this Machine holds no Sealing Key".

1. On that server, run `sudo ployz recover`, or from your computer
   `ployz recover --connect ssh://root@203.0.113.10`.
2. Type your recovery code when it asks. The prompt doesn't show what you type. A script can
   pipe the code in with `--code-stdin` instead.
3. Deploy again.

<!-- TODO(#3387): once a lone recovered server lets you add new sealed values, say so here. -->

Servers you add after that get the key from this one. If the code is wrong, `ployz recover`
says so and changes nothing, so you can try again.

## If you have no recovery code

If you lose every server and have no code, no one can open the values you sealed for them,
Ployz included.

- **Your organization never had a code.** The next first server makes a new key, and gives you
  a new code as above. Values sealed with the old key fail to deploy, and the error names the variable
  and says it "does not open with this Cluster's Sealing Key". Delete each one and add it
  again with its value.
- **A server waits in Needs recovery, and `ployz recover` finds no backup to recover from.**
  Your servers were added before Ployz Cloud stopped expecting a key it had no backup of. Stop
  Ployz on the waiting server with `sudo systemctl stop ployz.socket ployz`, since Ployz won't
  forget a server that still answers. Then
  [forget your servers](manage-servers.md#forget-servers-you-deleted) once more and add one
  again. It starts with a new key and a new code, as above. To add the same server again, first
  start Ployz on it with `sudo systemctl start ployz.socket ployz`, then add `--reset`, as in [The server is already initialised](../troubleshooting/servers.md#the-server-is-already-initialised).
- **You had a code but lost it.** There is no way to reset it, and the values you sealed are
  gone for good. A new server keeps waiting in **Needs recovery** for that code, and forgetting
  it again doesn't help. On ployz.dev, contact Ployz support for help getting going again, and
  plan to set every sealed value again. Rotate the code while you still have a server, so this
  never happens.

Next: [seal a secret](../services/variables.md#seal-a-secret).
