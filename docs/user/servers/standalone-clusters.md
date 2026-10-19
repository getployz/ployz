---
title: Run servers without Ployz Cloud
description: Found a standalone cluster over SSH, and choose which one the CLI talks to with contexts.
---

A standalone cluster is a group of your servers that the CLI runs over SSH, with no Ployz
Cloud organization behind it. Your apps run the same way, but there's no dashboard, and nothing
Ployz Cloud does for you, such as deploys from GitHub, reaches these servers.

The CLI keeps one **context** per standalone cluster on your computer. A context names the
cluster and lists the servers the CLI can reach it through. `ployz login` doesn't make one.
Servers in your organization need no context, because the CLI reaches them through Ployz Cloud.

## Start a standalone cluster

Each server needs the [requirements and firewall ports](add-a-server.md#what-you-need), and
your SSH key must log in to it without a password prompt.

1. Run `ployz server add --standalone root@203.0.113.10`. Ployz installs itself over SSH and
   starts the cluster on that server.
2. The CLI saves a context named `default` and switches to it. To give it another name, add
   `--context home-lab`.

Leave out the destination to start the cluster on the computer you're running the command on.
The command takes the same `--name`, `--storage`, `--ssh-key` and `--yes` flags as
`ployz server add`.

## Add more servers

Run `ployz server add --standalone root@203.0.113.11`. With a current context, the server joins
that context's cluster, and the context gains a way to reach it. Add `--context home-lab` to
join a different one.

To start a second cluster instead, name a context that doesn't exist yet:

```sh
ployz server add --standalone root@203.0.113.20 --context staging-lab
```

## Choose a context

```sh
ployz ctx ls            # every context, the current one marked
ployz ctx use home-lab  # make home-lab current
ployz ctx rm staging-lab
```

`ployz ctx use` without a name lets you pick one in a terminal. A context lists every server
the CLI can reach the cluster through. The CLI tries them in order and moves to the next one
when a server doesn't answer. To try another server first, run
`ployz ctx use home-lab --connection 2`, with its number or its name from
`ployz ctx ls --json`.

`ployz ctx rm` deletes only the entry on your computer. The servers and your apps keep running.

## Pick where one command acts

Add `--context home-lab`, or `-c home-lab`, to any command to run it against that cluster
once, without changing the current context. `PLOYZ_CONTEXT=home-lab` does the same for every
command in your shell.

`--connect root@203.0.113.10` skips contexts and talks to that one server for one command.
`PLOYZ_CONNECT` sets it for your shell. The `ployz ctx` commands refuse `--connect`, since they
only change your local contexts.

Without either flag, a signed-in CLI acts through Ployz Cloud. Signed out, it uses the
current context, or the Ployz on the server you're running the command on.

## Good to know

- **Contexts live in one file.** It's `~/.config/ployz/config.yaml`. Set `PLOYZ_CONFIG` to
  keep it somewhere else.
- **Copy the file to use another computer.** Only `ployz server add --standalone` writes a
  context, and running it against a server already in a cluster offers to reset that server.
