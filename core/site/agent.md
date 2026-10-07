# Set up Ployz

These steps install the Ployz CLI, teach your coding agent how to use it, and sign you in to
Ployz Cloud. Run them in order. Each step is safe to run again.

## 1. Install the CLI

```sh
curl -fsSL https://ployz.sh | sh
```

This installs `ployz` to `/usr/local/bin`. If that directory isn't writable, the script runs
`sudo`, which may ask the user for a password. To install without `sudo`, use a directory on
the user's `PATH` instead:

```sh
mkdir -p ~/.local/bin
curl -fsSL https://ployz.sh | INSTALL_BIN_DIR="$HOME/.local/bin" sh
```

Check it with `ployz --version`. Homebrew users can run `brew install getployz/ployz/ployz`
instead.

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

## 4. Finish

Tell the user:

> Ployz is set up. The CLI is installed, the Ployz skill is in ~/.agents/skills/ployz, and
> you're signed in. Restart or reload your agent so it picks up the skill.

Name `~/.claude/skills/ployz` as well if step 2 wrote it, and leave out any step that didn't
succeed, with what went wrong.
