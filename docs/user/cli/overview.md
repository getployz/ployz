---
title: CLI and coding agents
description: Install the ployz CLI, set up your coding agent, deploy a directory and use tokens in CI.
---

The `ployz` CLI does what the dashboard does, from your terminal. Install it, give your coding
agent the Ployz skill, and the agent can deploy and change your services for you.

## Install the CLI

```sh
curl -fsSL https://ployz.sh | sh

# or with Homebrew
brew install getployz/ployz/ployz
```

It runs on Linux and macOS. On Windows, install it inside WSL. To upgrade, run the same command
again, or `brew upgrade ployz`.

## Sign in

1. Run `ployz login`. It prints a code and opens your browser.
2. Sign in with GitHub if the page asks.
3. On **Sign in the Ployz CLI**, check that the code matches your terminal, then click
   **Approve**.

`ployz logout` signs this computer out.

Commands act on your only project and its default environment. With more than one, add
`--project my-app --env production`, or run `ployz link --project my-app --env production` once in
your app's directory.

## Set up your coding agent

```sh
ployz setup agent
```

This installs the Ployz skill for coding agents, including Claude Code. Your agent learns every
command and setting from it, `ployz --help` and `ployz schema --json`, so you can ask it to
deploy, add a database or read logs.

Run it again after you upgrade the CLI; `ployz --help` tells you when the skill is out of date.
An agent can also sign you in: it runs `ployz login`, and you click **Approve** on the link it
gives you.

## Deploy a directory

> [!WARNING]
> `ployz up` uploads every file in the directory except `.git`, including files Git ignores
> such as `.env` and `node_modules`. Run it from a fresh clone, or move secrets out of the
> directory first.

From your app's directory:

```sh
ployz up
```

The first time, Ployz creates a project named after the directory, adds a service with an https
address, builds your app, deploys it and prints the address.

An upload can be at most 256 MiB compressed. To deploy on every push instead, see
[Deploy from GitHub](../deploy/github.md).

## Make an organization token

CI jobs and scripts sign in with a token instead of a browser. Set it as `PLOYZ_TOKEN`:

```sh
# On your computer: make a token. It's shown once, so copy it.
ployz token new ci

# In CI, with the token stored as the secret PLOYZ_TOKEN
ployz up --project my-app --env production
```

Always pass `--project` in CI, or `ployz up` creates a new project named after the checkout.

The token acts as you in this organization: whoever holds it can deploy and run commands in your
services. It expires after 90 days unless you pass `--expires-in` (1 to 365 days). To revoke it,
find its ID with `ployz token ls`, then run `ployz token rm ID`. If it names a server that's
offline, run it again once that server is back.

Add `--json` to any command to get one JSON object instead of text. If the command fails, that
object is `{"error": {...}}`, with the next command to run in `details` when there is one.

Scripts can tell failures apart by exit code:

| Exit | Meaning |
| --- | --- |
| 0 | Done. |
| 1 | It failed: something wasn't found, a server couldn't be reached, or Ployz refused. |
| 2 | Fix the command: a bad argument, a missing `--confirm`, or a name that matches more than one thing. |
| 3 | You got a result, but some servers didn't answer, so part of it is missing. Run it again once they're back. |

`ployz exec` exits with your command's own exit code.

Ployz colors its output in a terminal. Set `NO_COLOR=1` or pass `--color never` to turn color
off, or `--color always` to keep it when you pipe the output.
