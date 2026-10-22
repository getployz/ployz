---
title: CLI and coding agents
description: Install the ployz CLI, set up your coding agent, deploy a directory and use tokens in CI.
---

The `ployz` CLI does what the dashboard does, from your terminal. Install it, give your coding
agent the Ployz skill, and the agent can deploy and change your services for you.

## Install the CLI

```sh
curl -fsSL https://ployz.sh | sh
```

It runs on Linux and macOS. On Windows, install it inside WSL. To upgrade, run the same command
again.

The line installs the stable release to `/usr/local/bin`. For the beta channel, end it with
`sh -s -- beta`. For one version, name it after `sh -s --` instead. To install somewhere else, set
`INSTALL_BIN_DIR` to a directory that exists:

```sh
curl -fsSL https://ployz.sh | INSTALL_BIN_DIR="$HOME/.local/bin" sh
```

## Sign in

1. Run `ployz login`. It prints a code and opens your browser.
2. Sign in with GitHub if the page asks.
3. On **Sign in the Ployz CLI**, check that the code matches your terminal, then click
   **Approve**.

This computer starts in the organization the page names. Switching organization in the dashboard
later doesn't move it, and `ployz org use ORG` does. `ployz logout` signs this computer out.
If Cloud can't be reached, or refuses because you've left the sign-in's organization, `ployz logout`
still forgets the sign-in here and warns that Cloud may still honour it until you remove it with
`ployz token rm`. An unreadable sign-in is forgotten too, and `ployz login` replaces it. While it
is unreadable, `PLOYZ_TOKEN` also needs `PLOYZ_CLOUD_URL`.

The sign-in is saved in `~/.config/ployz`, or in `$XDG_CONFIG_HOME/ployz` when you set
`XDG_CONFIG_HOME`. Set `PLOYZ_CONFIG` to a file path to keep a separate one.

Commands act on your only project and its default environment. With more than one, add
`--project my-app --env production`, or run `ployz link --project my-app --env production` once in
your app's directory.
`ployz ctx` shows where commands act from here. In a directory with no project linked, it asks Cloud
which projects there are: it names your only project, or says the organization has several and
how to link one. If Cloud doesn't answer within a few seconds, it shows `(none)` and the same
`ployz link` step.

A link belongs to the organization it was made in. Acting in another one, a command that uses the
link, such as `ployz status`, refuses. When both are Ployz Cloud organizations, it names
`ployz org use ORG` to switch back. When either is the local organization, it names `ployz link`
to link the directory where you act now. If the organization the link was made in was replaced by
a new one with the same name, it says the directory `is linked in an
earlier Organization ORG, which another Organization of that name replaced` and names
`ployz link` instead: `org use` can't reach the old one, so link the directory again.

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
ls`, `show`, `approve` and `deny`, `ployz uptime show` and `check`, and the team commands below.

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
if the directory isn't linked. A server that doesn't answer Ployz Cloud is named, and the command
exits 3, as it does when Ployz reads your servers itself. A `deploy` token reads only its own environment, so
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
> In a Git checkout, `ployz up` uploads only the files Git keeps, tracked or not, and leaves out
> `.git` and every file Git ignores, such as `.env` and `node_modules`. It stops if `git` can't
> list them. If you committed `.env` before, Git still keeps it, so add it to `.gitignore` and
> run `git rm --cached .env`. Outside a Git checkout it uploads every file in the directory, so move secrets out
> of it first.

From your app's directory:

```sh
ployz up
```

The first time, Ployz creates a project named after the directory, adds a service with an https
address, builds your app, deploys it and prints the address. In an empty directory it stops
before it creates anything, with `Nothing to deploy` and exit code 2.

An upload can be at most 95 MiB compressed. To deploy on every push instead, see
[Deploy from GitHub](../deploy/github.md).

`ployz github ls` lists the GitHub accounts your organization is connected to, and the link to
connect another. `ployz github disconnect <login>` disconnects one from this organization only.

## Make an organization token

CI jobs and scripts sign in with a token instead of a browser. Set it as `PLOYZ_TOKEN`:

```sh
# On your computer: make a token that can deploy only to staging. It's shown once, so copy it.
ployz token new ci --preset deploy --env staging

# In CI, with the token stored as the secret PLOYZ_TOKEN
ployz up --project my-app --env staging
```

Always pass `--project` in CI, or `ployz up` creates a new project named after the checkout.
If no service in that project already builds from an upload, `ployz up` adds one named after
the directory and builds it, as it does in a new project. It stops and says why, adding no
service and linking nothing, when a service builds from Git (run `ployz deploy` to ship it) or
when a service that pulls an image already has the directory's name.

Ployz ignores spaces and newlines around `PLOYZ_TOKEN`, as a paste into a secret or `.env` file
often leaves. A value that is still not a token, such as one with a newline inside it, fails with
`PLOYZ_TOKEN is not a token`, and Ployz never prints it.

A token acts as you in this organization, narrowed by its preset:

| Preset | What it can do |
| ------ | -------------- |
| `full` (the default) | Everything you can, and it follows your role if it changes. |
| `read` | Read what you can read. It can't change anything. |
| `deploy` | Read what you can, and stage and deploy changes to the one Environment `--env` names, in any project. That Environment must already exist in one of your projects. If it's Protected, only an owner or admin can make one. |

A deploy token that tries to change another Environment is refused and nothing changes. With the token above, `ployz up --project my-app --env production`
fails with `This token can deploy only to staging.` A token refused for its preset names what it lacks, such as
`This token's grant doesn't include servers:shell.` A token never does more than your role allows. Tokens made before presets existed
are `full`. You can also make, see and revoke tokens under **Organization → Tokens** in the
dashboard.

Whoever holds a `full` token can deploy and run commands in your services. A token expires after 90 days unless you pass `--expires-in` (1 to 365 days), and stops
working at once if you leave the organization, are removed from it, or the organization is
deleted. Then unset `PLOYZ_TOKEN` and run `ployz login`, or make a new token in an organization
you're still in. To revoke it,
find its ID with `ployz token ls`, then run `ployz token rm ID`. If it names a server that's
offline, run it again once that server is back. Owners and admins see and can revoke every
organization token in the organization, not only their own. Members see only theirs.

Only your login can make or revoke tokens. A token can't do either, so a leaked one can't make
another that outlives it, or revoke the rest. While `PLOYZ_TOKEN` is set, Ployz acts with the token
even after `ployz login`: run `unset PLOYZ_TOKEN && ployz login` before you make or revoke one. `ployz token ls` shows each token's SCOPE, such as `Deploy staging`, and its KIND: `cli` for
one made with `ployz token new`, `agent` for the one `ployz mcp login` made. It also lists each
computer where you ran `ployz login` as KIND `device`, named for the day it signed in, such as
`ployz login 2026-10-18`. `ployz token rm ID` signs that computer out. An agent token can't
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

`ployz status` lists what needs you under **needs attention**: a deployment that didn't complete,
a reference to a removed service, or `Server web-1 isn't answering` when none of the servers a
deploy would reach answers. With `--json` these are the `attention` rows; that one has reason
`no_server_answers` and names the servers in `servers`. It comes from the same single connection
status makes to check your servers (below), not a separate one. A server that accepts the
connection but never answers takes 5 seconds to give up on, and status tries each server once.
A check that can't finish adds no row and warns `Could not check which copies serve or which
Servers picked up the new copies` instead, so no row doesn't prove a server answered. A context or `--connect` that reaches the daemon on this host through its local
socket never gets this row; the **this server** line covers it. Signed in to Cloud, status asks
Cloud for server access once per run. The first `ployz status` or other server command for a
sign-in or token also waits while Cloud sets up your access on each server.

`ployz status` works signed out too. It shows what it can and says `signed in as no one`. On a
server waiting for approval, it leads with a **this server** line: `Installing Docker`,
`Building ZFS`, `Waiting for approval`, `No cluster yet`, `Blocked`, `Rejected`, `Token expired`, `Token revoked`,
`Can't reach Cloud`, `Clock is off` or `Needs a newer ployz`. Then it says when the server last checked in and lists anything that blocks
it as `blocked` lines. With `--json` this is the `this_server` object. After `ployz server rm`
resets a server that joined from a Fleet Token, its **this server** line says `Not in a Cluster`
and `this_server` adds `"left": true`.

When a server is up but its proxy hasn't picked up a service's current replicas after a deploy,
`ployz status` names it, one line per service, like
`web = web-2 has not picked up the new copies. Check its network.` When a server's proxy sends a
replica's traffic to an address the replica's own server doesn't report, status names that
server too, like
`web = web-3 routes a copy to 10.210.2.2, which its owner does not report. Check its network.`
With `--json` these rows have reasons `routes_not_acked` and `routes_stale_address`. A server
that's down shows as **Down** instead. See
[A server hasn't picked up the new version](../troubleshooting/deployments.md#a-server-hasnt-picked-up-the-new-version)
and
[A server routes a replica at an old address](../troubleshooting/deployments.md#a-server-routes-a-replica-at-an-old-address).

When a service serves fewer replicas than its last deploy gave it, `ployz status` names it and
each replica that isn't serving, in the words `ployz ps` uses, like
`web = Serving 1 of 3 copies. web-1 restarting, web-2 draining.` A replica can be restarting,
exited, unhealthy, starting, or draining. A starting replica shows as `running, starting` while it
is still inside its health check's start period, and the line clears once the check passes. A
draining replica is out of routing but still running. With `--json` the row has reason
`copies_not_serving`, the `service`, how many replicas are `serving`, how many it `wanted`, and
`copies`, each with its `machine`, `container`, `rotation` and `runtime`. `wanted` is how many of
the replicas status could see should be serving. A server that is down or didn't answer isn't
seen. A down server shows as **Down**, and status warns about one that didn't answer. Its `next`
is `ployz logs web` when a named replica is still in routing, else `ployz deploy web`. Old
replicas still draining beside a full set of new ones, as just after a deploy, add no row. While
a deployment runs, status skips this check and warns that it did.

This check has limits:

- A replica removed outside Ployz, for example with `docker rm`, is unseen, so status can't name
  it.
- A global service's draining replica isn't named.
- While a server that has joined is down, or any server didn't answer, a draining replica isn't
  named.
- After a scale-down that changed only the replica count, a replica left draining can be named
  until the next deploy removes it.

See
[Status says a service serves fewer copies than it should](../troubleshooting/deployments.md#status-says-a-service-serves-fewer-copies-than-it-should).

A `read` or `deploy` token without server access doesn't show these lines yet. To check this, `ployz status`
reaches your servers the way `ployz ps` does, so it takes the same `--context` and
`--ssh-timeout` options.

`ployz server enroll 'host-*' --name 'web-{n}'` prints Cloud's enrollment plan and asks before
starting. Add `--yes` to accept it without a prompt, including with `--json`. `--status` follows
the stages. Run `ployz server enroll --status` to resume following after Ctrl-C. See
[Servers waiting to join](../servers/manage-servers.md#servers-waiting-to-join) for storage and
selection flags.

Human output names things: Servers by name and Deployments as `#3`. It shows a short Container
ID only where two rows would otherwise read the same. `--json` keeps every ID.

Add `--json` to any command to get one JSON object instead of text. A stream, such as `ployz logs`,
prints JSON Lines instead: one object per line. If the command fails before it prints anything,
that object is `{"error": {...}}`. A stream that fails after it printed names the failure on stderr
and exits with code 3. Its `cause` lists every underlying error, outermost first, and
`details` holds the next command to run when there is one. When a Ployz Cloud command such as
`ployz org member rm` refuses, `details` also says whether it wrote anything (`writes`), and for
an input it can't take, the JSON Schema `expected`. `field` names the input that is wrong. It is
left out when the input isn't a JSON object, and when it has a name the command doesn't take,
because Cloud never repeats that name.

Scripts can tell failures apart by exit code:

| Exit | Meaning |
| --- | --- |
| 0 | Done. |
| 1 | It failed: something wasn't found, a server couldn't be reached, a deploy failed, Ployz refused, the server or Cloud doesn't support what you asked, or the output couldn't be written, as with `> /dev/full`. |
| 2 | Fix the command: a bad argument, a flag the command doesn't take, a missing `--confirm`, or a name that matches more than one thing. |
| 3 | You got a result, but part of it is missing or didn't happen: some servers didn't answer, the output stopped partway, or some work couldn't be done. Examples: a `ployz server drain` that leaves a service where it is because no other server can take it, a `ployz env rm`, `ployz project rm` or `ployz env shutdown` whose removal didn't finish, a deploy that applied but couldn't see every server or left an old container running, or a deploy a newer one replaced or someone cancelled. A deploy that failed exits 1. The output says what's left. Run it again once the servers are back, or once there's somewhere for the work to go. |
| 130 | You cancelled: pressed Ctrl-C, didn't confirm a prompt, or stopped waiting for an approval. Before the work starts, nothing changes. A `ployz deploy` following its Deployment also cancels this way on SIGTERM, which CI runners, `timeout` and container stops send. A `ployz deploy` cancelled after its services applied keeps them, and its `--json` says `applied`. Once Ployz Cloud runs it, as with a `server rm` or `server upgrade` it's following, Ctrl-C only stops the following: the work goes on in Cloud and finishes on its own. A `server upgrade` run from your computer works the same way: the server goes on with the upgrade it took, and `ployz server show` says how it ended. |

`ployz exec` exits with your command's own exit code. When the container stops under it,
because a deploy replaced it or the service was stopped, it exits 1 with
`The container for web stopped; exec ended.` A `retry:` line follows when another container of the
service runs, and running it again attaches there.

A change can reach Ployz Cloud even when its answer never comes back. Then `ployz set`,
`service add` and other changes exit 1 with `Cloud ... stopped answering after it got this
change, so it may have applied.` Its `inspect:` line names the read that shows whether it
landed, such as `ployz diff` or, for a deploy, `ployz deployment ls`. Check there before you
run it again. With `--json` the error's `details` says `"writes": "unknown"`. When Cloud
can't be reached at all, the error says `Could not reach Cloud` and nothing changed.

A command that deletes something, such as `ployz project rm shop`, shows what goes and asks you
to type its name. Anything else, or Ctrl-C, cancels. Without a terminal, in CI, or with
`--json` it never asks: it exits 2 and prints the command to run instead, which passes the name
with `--confirm`. Commands that would otherwise ask you to choose, such as `ployz ctx use` without
a name or the storage choice in `ployz server add`, work the same way. A `deploy`, with or without
`--skip-deploy`, that needs an approval asks the same way in a terminal. Without one it waits for the approval in
Ployz Cloud, and with `--json` it exits 1 with `approval_required`. See
[Destructive actions ask first](../account/organizations.md#destructive-actions-ask-first).

Ployz reviews a `ployz env rm` or `ployz project rm` before it runs when it takes a deployed
service or volume off your servers. The name you confirm with binds that review: the command says
what goes, such as `Removing shop/qa removes Service web.`, and removes it at the version it
reviewed. To bind a version yourself, pass `--expect-version`: the removal refuses if anything
changed since, and names the version it reviewed now. The version binds only a removal that Ployz
reviews before it runs. Removing an environment with nothing deployed, such as a new empty
project, doesn't compare it, so it goes ahead even when the version you pass is stale.

`ployz env rm` and `ployz project rm` take what runs off your servers and wait for it. If a server
doesn't answer, the command returns with the environment still being removed, names the server,
and exits 3. Ployz Cloud tries again each hour, so it finishes within the hour after the server
answers or you remove it with `ployz server rm`. To finish it sooner, run the same `ployz env rm`
or `ployz project rm` again: it needs no new `--expect-version`, but still asks for approval as
the first run did. With `--json` it reports `removal: removed` only once the environment is gone
from `ployz env ls`, or for `project rm` the project from `ployz project ls`. Until then it reports
`removal: removing`, with the removal's `id` and the removal deployments it ran in `deployments`.
When one of them didn't apply, it also reports that deployment in full under `deployment` (its
`status`, `outcome` and `nodes`) and `next`: the command that finishes it, with the same
`--ployz-config` when you passed one. Each server it waits on is under `waiting_on`, by its name
in `server` and its machine ID in `server_id`.

Ployz colors its output in a terminal. With `CI` set or `TERM=dumb`, it prints plain text even in
one. Set `NO_COLOR=1` or pass `--color never` to turn color off, or `--color always` to keep it
when you pipe the output. A piped list stays plain tab-separated text either way. Put `--color` before `--`, and before the command `ployz exec` runs;
after either, it belongs to that command.
