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

Paste this into your coding agent, such as Claude Code or Codex:

```text
Set me up for Ployz: https://ployz.sh/agent.md
```

The agent installs the CLI and the Ployz skill, signs you in, and registers the Ployz MCP tools.
To sign in, it gives you a link and a code, and you click **Approve**. Restart or reload the
agent afterwards so it loads the skill and the tools.
From then on you can ask it to deploy, add a database or read logs. The skill points the agent
to `ployz --help` and `ployz schema --json` for every command and setting.

To set it up by hand, follow the steps in [ployz.sh/agent.md](https://ployz.sh/agent.md)
yourself. Run step 2 again to update the skill.

### Give the agent Ployz tools

`ployz mcp` serves the Cloud commands to your agent as MCP tools. The setup prompt above
registers it. To add it by hand for every project:

```sh
claude mcp add -s user ployz -- ployz mcp
```

For Codex, run `codex mcp add ployz -- ployz mcp`. Each tool runs one `ployz` command with
`--json`, signed in as you, and returns its one result. Tools that can remove something live,
such as `deploy` and `server rm`, are marked destructive. Of those, every tool that acts the
moment it runs, rather than through a plan Ployz reviews first, also asks the agent's app to
check with you before every call, even when you've told it to skip permission prompts.
`server rm` and `service stop` are two of them; `deploy` and `publish` are not. Claude Code
honors this.

When a `publish` or `deploy` needs your approval (see
[Ask before destructive actions](../account/organizations.md#ask-before-destructive-actions)),
the agent's app shows you what it would remove and asks you to approve or deny it, with an
optional reason. Approve and the command runs. Deny and the agent is told your reason and that
nothing changed. Close the dialog without answering and the approval stays pending in Ployz
Cloud. An app that can't show this dialog gets an error instead, naming the approval, which you
can approve in Ployz Cloud or by running the command yourself in a terminal. The 30-minute limit
below includes the time the dialog waits for you.

Some commands are not tools. Local commands such as `login` and `ctx use` act on this computer.
Servers are added with `ployz server add` from a terminal, not through MCP, because it installs
Ployz over SSH or on this computer. `exec` is interactive, and `service port-forward` stays open
until you stop it. Run these yourself. The tools also leave out the arguments that read stdin
and the ones that keep a command waiting, such as `logs --follow` and `volume sync --wait`.

New secrets can't be added through MCP. `set SERVICE --from-env-file PATH` does replace the
values of secrets the service already has, and they stay secret. To add a secret, run
`ployz set web.env.KEY --secret` yourself.

A tool call that runs for more than 30 minutes is stopped, along with the processes it started
on this computer, except a process that leaves the call's process group, for example with
`setsid` or `setpgid`. Calls still running
when the agent closes `ployz mcp` stop the same way. A deployment or volume run a call started
keeps going in Cloud, and the tool's error names the command that shows where it stands, such as
`ployz status`. When `ployz mcp` reaches Servers over SSH, through an SSH context or
`--connect ssh://`, the shared SSH connection is one such process: it stays open for up to 10
idle minutes so the next command reuses it.

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

When a command fails, Ployz prints what went wrong, then a `cause:` line with the underlying
error, such as the operating system's or the server's own message.

A command prints its result on stdout and everything else on stderr: progress, warnings, the
`next:` line, prompts and errors. `ployz service ls > services.txt` saves only the table.

A list is an aligned table in a terminal. Piped, it is tab-separated with an uppercase header
row, so `ployz service ls | cut -f1` prints the names. An empty list prints one sentence on
stderr, such as `No Services in production yet.`; piped, it also prints the header row on stdout.
A single record, such as
`ployz status`, prints one `label = value` line per field when piped.

Human output names things: Servers by name and Deployments as `#3`. It shows a short Container
ID only where two rows would otherwise read the same. `--json` keeps every ID.

Add `--json` to any command to get one JSON object instead of text. If the command fails, that
object is `{"error": {...}}`. Its `cause` lists every underlying error, outermost first, and
`details` holds the next command to run when there is one.

Scripts can tell failures apart by exit code:

| Exit | Meaning |
| --- | --- |
| 0 | Done. |
| 1 | It failed: something wasn't found, a server couldn't be reached, Ployz refused, or the server or Cloud doesn't support what you asked. |
| 2 | Fix the command: a bad argument, a flag the command doesn't take, a missing `--confirm`, or a name that matches more than one thing. |
| 3 | You got a result, but some servers didn't answer, so part of it is missing. Run it again once they're back. |
| 130 | You cancelled: pressed Ctrl-C, didn't confirm a prompt, or stopped waiting for an approval. Nothing was removed. |

`ployz exec` exits with your command's own exit code.

A command that deletes something, such as `ployz project rm shop`, shows what goes and asks you
to type its name. Anything else, or Ctrl-C, cancels. Without a terminal, in CI, or with
`--json` it never asks: it exits 2 and prints the command to run instead, which passes the name
with `--confirm`. Commands that would otherwise ask you to choose, such as `ployz ctx use` without
a name or the storage choice in `ployz server add`, work the same way. A `publish` or `deploy`
that needs an approval asks the same way in a terminal. Without one it waits for the approval in
Ployz Cloud, and with `--json` it exits 1 with `approval_required`. See
[Ask before destructive actions](../account/organizations.md#ask-before-destructive-actions).

Ployz colors its output in a terminal. With `CI` set or `TERM=dumb`, it prints plain text even in
one. Set `NO_COLOR=1` or pass `--color never` to turn color off, or `--color always` to keep it
when you pipe the output. A piped list stays plain tab-separated text either way. Put `--color` before `--`, and before the command `ployz exec` runs;
after either, it belongs to that command.
