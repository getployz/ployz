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

This computer starts in the organization the page names. Switching organization in the dashboard
later doesn't move it, and `ployz org use ORG` does. `ployz logout` signs this computer out.

Commands act on your only project and its default environment. With more than one, add
`--project my-app --env production`, or run `ployz link --project my-app --env production` once in
your app's directory.
`ployz ctx` shows where commands act from here.

## Set up your coding agent

Paste this into your coding agent, such as Claude Code or Codex:

```text
Set me up for Ployz: https://ployz.dev/agents.md
```

The agent installs the CLI and the Ployz skill, signs you in, and registers the Ployz MCP tools.
To sign in, it gives you a link and a code, and you click **Approve**. Restart or reload the
agent afterwards so it loads the skill and the tools.
From then on you can ask it to deploy, add a database or read logs. The skill points the agent
to `ployz --help` and `ployz schema --json` for every command and setting.

To set it up by hand, follow the steps in [ployz.dev/agents.md](https://ployz.dev/agents.md)
yourself. Run step 2 again to update the skill.

### Give the agent Ployz tools

An agent app can also use the Ployz tools without the CLI; see
[Connect an agent to Ployz Cloud](../account/connect-an-agent.md).

`ployz mcp` gives your agent Ployz tools over MCP. The setup prompt above registers it. To add it
by hand for every project:

```sh
claude mcp add -s user ployz -- ployz mcp
ployz mcp login
```

For Codex, run `codex mcp add ployz -- ployz mcp`. `ployz mcp` doesn't use your sign-in. It acts
with its own agent token, which `ployz mcp login` makes in your current organization and keeps
on this computer. Until you run it, every tool call fails and names the command. The token acts
as you in that organization, expires after 365 days, and shows as `agent` under KIND in
`ployz token ls`. Running `ployz mcp login` again makes a new one and revokes the old. An agent
token can't answer an approval, so the agent can't approve its own calls through `ployz mcp`.
When `ployz mcp` asks you, your answer goes with your own sign-in. An agent that can run commands
in your terminal still acts with your sign-in there, so don't let it run `ployz org approval`
for you. With `PLOYZ_TOKEN` set, `ployz mcp` uses that token instead, and Ployz still treats its
calls as the agent's.

The tools create
Projects, read and stage changes in them, review and deploy them (or save them for the next deploy), restore or undo a
saved version, list, follow, redeploy, start and cancel their deployments, show Services, Servers
and Volumes, and read a Service's recent logs. The agent can resend a call after a lost reply
without running it twice. Each call acts where the agent's directory is linked when the call
starts, so after `ployz link` or `ployz org use` the next call follows it.

Some things are not tools. Run these yourself in a terminal:

- `login`, `ctx use` and other commands that act on this computer.
- `server add`, which installs Ployz over SSH or on this computer.
- `server enroll`, which enrolls hosts waiting in Cloud by name or glob.
- `exec`, `service port-forward`, `logs --follow` and `up`.
- Branches, tokens and Fleet Tokens (`server token`).
- Secrets and registry credentials. `ployz set web.env.KEY --secret` adds a secret.

Tools that can delete something when they run are marked destructive: `deploy`,
`deployment_retry`, `deployment_start`, `discard`, `environment_remove`, `project_remove`, the
server commands that drain, clean, forget or remove a server, the Volume commands that move,
mirror, release or restore a copy or remove a mirror, and removing an organization member or
invite. Staging a removal, such as `service_remove`, deletes nothing until the next deploy, so it
isn't marked. When one of them needs your approval (see
[Destructive actions ask first](../account/organizations.md#destructive-actions-ask-first)),
the agent's app shows you what it would remove and asks you to approve or deny it. Your answer
goes to Ployz Cloud with your own sign-in, not the agent's token. Removing a
Project, an Environment or a Server also asks you to type its name. Approve and the call goes
ahead. Leave Approve unticked and Ployz denies the approval; any reason you type goes back to the
agent, which is told nothing changed. Declining or closing the dialog denies it too, without a
reason. Answer within 30 minutes of the agent's call; a later answer isn't recorded and the
approval stays pending in Ployz Cloud.

In an app that can't show this dialog, the call fails right away with `approval_required`,
what it would remove, the approval's ID and a link to your organization. Open the link,
approve or deny it under **Waiting on you** in the Ployz agent, then tell the agent to try
again. It calls the tool again with that approval: an approved call goes ahead, and a denied
one fails, with your reason if you gave one. The agent can never answer the approval itself.

`ployz mcp` stops a call that runs for more than 30 minutes. A deployment or Volume run it
started keeps going in Cloud, and the error names the tool that shows where it stands. A call
stopped while Cloud had not yet answered its change says the change may have landed, and the
agent can resend it without running it twice.

## Search the docs

`ployz docs` searches these docs from your terminal, offline. Ask a question and it prints the
five sections that match best, each with its page and an excerpt:

```sh
ployz docs add a custom domain
```

Name a page instead to print it whole, as Markdown: `ployz docs services/domains.md`, or just
`ployz docs domains.md`. With `--json`, a search returns `results` with each section's `path`,
`anchor` and `excerpt`, and a page returns its `content`. Your coding agent gets the same search
as the `docs` tool from `ployz mcp`.

Many commands' `--help` ends with the page that shows them: `ployz config put --help` ends
with `Docs: ployz docs services/configs.md`. A command that needs a role's permission names it
there too: `ployz deploy --help` ends with `Needs changes:deploy`, and a member of the wrong role
who runs it gets `Needs changes:deploy` and exit code 1. See
[Invite people](../account/organizations.md#invite-people) for what each role can do.

## Commands from Ployz Cloud

Some commands come from Ployz Cloud instead of the CLI itself, so a new one reaches you without
an upgrade. Today they include `ployz server upgrade-policy show` and `set`, `ployz org approval
ls`, `show`, `approve` and `deny`, and the team commands below.

When you're signed in, `ployz --help` lists them with the others, and they take `--json` like any
command. `ployz schema --json` marks each one `"origin": "cloud"`. The first one you run fetches
the list from Ployz Cloud and keeps a copy on this computer, so `--help` still shows them offline.
If Cloud no longer has a command, it fails with `unknown_command`; `ployz --help` then shows the
current list.

Your coding agent gets the ones meant for agents as tools from `ployz mcp`, and its tool list
follows Ployz Cloud without a restart. Commands that ask you to type a name, such as
`ployz org approval approve`, run only in a terminal. Without Ployz Cloud, none of these commands
exist.

## Manage your team

Owners and admins manage who belongs to the Organization this device acts in:

```sh
ployz org member ls
ployz org member invite bob@example.com --role admin
ployz org member role bob@example.com member
ployz org member rm bob@example.com
ployz org invite ls
ployz org invite revoke bob@example.com
```

`invite` prints `Invite link (expires in 7 days)` and the link. Send it to the person yourself.
Roles are `owner`, `admin` and `member`. Only an owner can make, change or remove an owner.
Any member can run `ployz org member ls`. Pending invites name who was invited, so
`ployz org invite ls` refuses anyone but an owner or admin.

Through Ployz Cloud, commands that talk to your servers directly need an owner or admin too: `exec`,
`service start`, `service stop`, `service restart`, `service port-forward`,
`server ls`, `server show`, `server logs`, `server set`, `server drain`, `server upgrade`,
`server rm`, `server forget <server>` and `server clean` without a namespace. A member gets
`Server access needs an admin. Ask an owner or admin of <organization>.` and exit code 1. A `read` or
`deploy` [token](#make-an-organization-token) can't run them either, even an owner's: it gets
`This token's grant doesn't include servers:shell.`

`logs`, `ps` and `metrics` work for anyone who can read logs and metrics, including members and
`read` and `deploy` tokens. Without server access they read through Ployz Cloud instead: `logs`
shows each server's newest 200 lines at most, with sealed variables scrubbed, and can't use `--follow`,
`--until`, `--machine` or a container name. Name the environment with `--project` and `--env`
if the directory isn't linked. A `deploy` token reads only its own environment, so
`ployz logs --env production` with a staging token fails with `This token can deploy only to staging.` See
[Reach a server from the CLI](../servers/manage-servers.md#reach-a-server-from-the-cli).

Pick a member by email or by name. When two members share a name, the command refuses with
`ambiguous`, lists both emails and exits 2, so name the one you mean by email.

`role`, `rm` and `revoke` show what they will change, and in which Organization, and ask before
they run. In a terminal you type the email it names, or the invite id for a link with no email, to
go ahead. Without a terminal they wait until someone approves them in Ployz Cloud. Through
`ployz mcp`, your agent gets back an approval to show you and changes nothing until you approve it. The dashboard's
Members page never asks. `ployz org member ls` and `ployz org invite ls` never ask.

`ployz org leave` takes you out of the Organization this device acts in, after you confirm. It
leaves that Organization even if you switched to another one in the dashboard since.
Pass `--yes` to skip the question. It then moves this device to another Organization of yours,
the way `ployz org rm` does, and prints which one.

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

An upload can be at most 95 MiB compressed. To deploy on every push instead, see
[Deploy from GitHub](../deploy/github.md).

`ployz github ls` lists the GitHub accounts your organization is connected to, and the link to
connect another. `ployz github disconnect <login>` disconnects one from this organization only.

## Make an organization token

CI jobs and scripts sign in with a token instead of a browser. Set it as `PLOYZ_TOKEN`:

```sh
# On your computer: make a token that can deploy only to staging. It's shown once, so copy it.
ployz token new ci --preset deploy --environment staging

# In CI, with the token stored as the secret PLOYZ_TOKEN
ployz up --project my-app --env staging
```

Always pass `--project` in CI, or `ployz up` creates a new project named after the checkout.

A token acts as you in this organization, narrowed by its preset:

| Preset | What it can do |
| ------ | -------------- |
| `full` (the default) | Everything you can, and it follows your role if it changes. |
| `read` | Read what you can read. It can't change anything. |
| `deploy` | Read what you can, and stage and deploy changes to the one Environment `--environment` names, in any project. That Environment must already exist in one of your projects. If it's Protected, only an owner or admin can make one. |

A deploy token that tries to change another Environment is refused and nothing changes. With the token above, `ployz up --project my-app --env production`
fails with `This token can deploy only to staging.` A token refused for its preset names what it lacks, such as
`This token's grant doesn't include servers:shell.` A token never does more than your role allows. Tokens made before presets existed
are `full`. You can also make, see and revoke tokens under **Organization → Tokens** in the
dashboard.

Whoever holds a `full` token can deploy and run commands in your services. A token expires after 90 days unless you pass `--expires-in` (1 to 365 days), and stops
working at once if you leave the organization or are removed from it. To revoke it,
find its ID with `ployz token ls`, then run `ployz token rm ID`. If it names a server that's
offline, run it again once that server is back. Owners and admins see and can revoke every
organization token in the organization, not only their own. Members see only theirs.

Only your login can make or revoke tokens. A token can't do either, so a leaked one can't make
another that outlives it, or revoke the rest. `ployz token ls` shows each token's SCOPE, such as `Deploy staging`, and its KIND: `cli` for
one made with `ployz token new`, `agent` for the one `ployz mcp login` made. An agent token can't
answer approvals, and a token's kind never changes. A token you make outlives the login you made it
with: `ployz logout` doesn't revoke it.

When a command fails, Ployz prints what went wrong, then a `cause:` line with the underlying
error, such as the operating system's or the server's own message.

A command prints its result on stdout and everything else on stderr: progress, warnings, the
`next:` line, prompts and errors. `ployz service ls > services.txt` saves only the table.

A list is an aligned table in a terminal. Piped, it is tab-separated with an uppercase header
row, so `ployz service ls | cut -f1` prints the names. An empty list prints one sentence on
stderr, such as `No Services in production yet.`; piped, it also prints the header row on stdout.
A single record, such as
`ployz status`, prints one `label = value` line per field when piped.

`ployz status` works signed out too. It shows what it can and says `signed in as no one`. On a
server waiting for approval, it leads with a **this server** line: `Installing Docker`,
`Building ZFS`, `Waiting for approval`, `Blocked`, `Rejected`, `Token expired`, `Token revoked`,
`Can't reach Cloud`, `Clock is off` or `Needs a newer ployz`. Then it says when the server last checked in and lists anything that blocks
it as `blocked` lines. With `--json` this is the `this_server` object.

`ployz server enroll 'host-*' --name 'web-{n}'` prints Cloud's enrollment plan and asks before
starting. Add `--yes` to accept it without a prompt, including with `--json`. `--status` follows
the stages. Run `ployz server enroll --status` to resume following after Ctrl-C. See
[Servers waiting to join](../servers/manage-servers.md#servers-waiting-to-join) for storage and
selection flags.

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
| 130 | You cancelled: pressed Ctrl-C, didn't confirm a prompt, or stopped waiting for an approval. Before the work starts, nothing changes. A `ployz deploy` cancelled after its services applied keeps them, and its `--json` says `applied`. Once Ployz Cloud runs it, as with a `server rm` or `server upgrade` it's following, Ctrl-C only stops the following: the work goes on in Cloud and finishes on its own. |

`ployz exec` exits with your command's own exit code.

A command that deletes something, such as `ployz project rm shop`, shows what goes and asks you
to type its name. Anything else, or Ctrl-C, cancels. Without a terminal, in CI, or with
`--json` it never asks: it exits 2 and prints the command to run instead, which passes the name
with `--confirm`. Commands that would otherwise ask you to choose, such as `ployz ctx use` without
a name or the storage choice in `ployz server add`, work the same way. A `deploy`, with or without
`--skip-deploy`, that needs an approval asks the same way in a terminal. Without one it waits for the approval in
Ployz Cloud, and with `--json` it exits 1 with `approval_required`. See
[Destructive actions ask first](../account/organizations.md#destructive-actions-ask-first).

`ployz env rm` and `ployz project rm` take what runs off your servers and wait for it. If a server
doesn't answer, the command returns with the environment still being removed, names the server,
and exits 3. Ployz finishes on its own once the server answers or you remove it with
`ployz server rm`. With `--json` it reports `removal: removed` only once the environment is gone
from `ployz env ls`, or for `project rm` the project from `ployz project ls`.

Ployz colors its output in a terminal. With `CI` set or `TERM=dumb`, it prints plain text even in
one. Set `NO_COLOR=1` or pass `--color never` to turn color off, or `--color always` to keep it
when you pipe the output. A piped list stays plain tab-separated text either way. Put `--color` before `--`, and before the command `ployz exec` runs;
after either, it belongs to that command.
