# Set up Ployz

These steps install the Ployz CLI, teach your coding agent how to use it, and sign you in to
Ployz Cloud. Run them in order. Each step is safe to run again.

## 1. Install the CLI

```sh
curl -fsSL https://ployz.sh | sh -s -- beta
```

This installs `ployz` to `/usr/local/bin`. If that directory isn't writable, the script runs
`sudo`, which may ask the user for a password. To install without `sudo`, use a directory on
the user's `PATH` instead:

```sh
mkdir -p ~/.local/bin
curl -fsSL https://ployz.sh | INSTALL_BIN_DIR="$HOME/.local/bin" sh -s -- beta
```

Check it with `ployz --version`. Install the beta, not Homebrew or the stable release. Ployz
Cloud enrolls servers only from 0.3.0-beta.6 on. The stable release and Homebrew are still on
0.2.

## 2. Install the Ployz skill

The skill tells coding agents how to use the `ployz` CLI. This writes it to
`~/.agents/skills/ployz`, and also to `~/.claude/skills/ployz` when Claude Code is installed:

```sh
mkdir -p ~/.agents/skills/ployz
curl -fsSL https://ployz.sh/skill.md -o ~/.agents/skills/ployz/SKILL.md
if [ -d ~/.claude ]; then
  mkdir -p ~/.claude/skills/ployz
  cp ~/.agents/skills/ployz/SKILL.md ~/.claude/skills/ployz/SKILL.md
fi
```

Running it again replaces the skill with the latest version.

## 3. Sign in

```sh
ployz login --json
```

If the result has `"status": "signed_in"`, the user is already signed in; go to step 4.
Otherwise it has a `url` and a `code`. Show both to the user and ask them to open the link,
check that the code matches, and click **Approve**. Then wait for the approval:

```sh
ployz login --wait
```

If the user has no Ployz account, or isn't there to approve, start in a sandbox instead. One call
gives you a token and an organization of your own, with no sign-in.

```sh
curl -fsS -X POST https://ployz.dev/api/agent/signup
```

Put the `token` from the reply before every `ployz` command, as in
`PLOYZ_TOKEN=ployz_... ployz deploy`. A command with the token acts in the sandbox. Set it on each
command, because a shell you open later doesn't keep an `export`. Give the user the `claimUrl`.
They open it and sign in with GitHub to keep what you built. Ployz starts removing the sandbox and
its projects 24 hours after the call unless someone keeps it. Custom domains, tokens, invites,
billing and GitHub wait for a claim. See https://ployz.dev/docs/account/try-without-an-account.

In a sandbox, `ployz mcp login` waits for a claim, because it makes a token. In step 4, skip it
and register `ployz mcp` with the sandbox token instead. If `ployz` is already registered, remove
it first with `claude mcp remove -s user ployz`.

```sh
claude mcp add -s user ployz -e PLOYZ_TOKEN=ployz_... -- ployz mcp
```

In Codex, run `codex mcp add ployz --env PLOYZ_TOKEN=ployz_... -- ployz mcp` instead, after
`codex mcp remove ployz` if it is already registered.

## 4. Add the Ployz tools

`ployz mcp` serves the Ployz tools over MCP. Register it with the agent you are running in:

```sh
claude mcp add -s user ployz -- ployz mcp
```

In Codex, run `codex mcp add ployz -- ployz mcp` instead. If the server is already registered,
leave it as it is.

Then give `ployz mcp` its own agent token. It doesn't use the sign-in from step 3, and its tools
fail until it has one:

```sh
ployz mcp login
```

Running it again replaces the token and revokes the old one.

An agent app that can't run `ployz` on this computer can use the Ployz tools from Ployz Cloud
instead. Add `https://ployz.dev/mcp` as a remote MCP server, for example
`claude mcp add --transport http ployz https://ployz.dev/mcp`. The app opens the browser for the
user to sign in and pick an organization. This works on ployz.dev only, not on a Self-hosted
Cloud.

## 5. Finish

Tell the user:

> Ployz is set up. The CLI is installed, the Ployz skill is in ~/.agents/skills/ployz, the
> Ployz tools are registered, and you're signed in. Restart or reload your agent so it picks
> up the skill and the tools.

Name `~/.claude/skills/ployz` as well if step 2 wrote it, and leave out any step that didn't
succeed, with what went wrong.

In a sandbox, the user isn't signed in. Replace "and you're signed in" with "and I'm working in a
sandbox", and give them the `claimUrl` to open within 24 hours.
