---
name: ployz
description: Deploy and operate apps on Ployz servers with the `ployz` CLI: sign in, add servers, add Services and change their Settings, review, publish and deploy, read logs. Relevant whenever a task mentions Ployz or the `ployz` command.
---

# Ployz CLI

This file tracks the latest Ployz release. The installed CLI is the source of truth for exact syntax:

- `ployz --help` and `ployz COMMAND --help` list the commands and their flags.
- `ployz schema --json` lists every command and Setting.
- `ployz explain SERVICE.SETTING` describes one Setting, with an example.
- `ployz mcp` serves the same Cloud commands as MCP tools. Register it with `claude mcp add -s user ployz -- ployz mcp` or `codex mcp add ployz -- ployz mcp`. A tool marked destructive can remove something live; confirm with the user first.

## Where commands act

- Commands act on your only Project and its default Environment. With more than one, pass `--project my-app --env production`, set `PLOYZ_PROJECT` and `PLOYZ_ENV`, or run `ployz link --project my-app --env production` once in the app's directory.
- `ployz status` shows who is signed in, where commands act, and what is staged, deploying or needs attention.
- `ployz login` signs in through the browser. With `--json` it prints the approval URL and code at once; show them to the user, then run `ployz login --wait`.
- `PLOYZ_TOKEN` authenticates without `ployz login`.

## Deploy and change Services

- `ployz up` deploys the current directory: it creates and links a Project if needed, uploads, builds and deploys. It uploads every file except `.git`, including files Git ignores such as `.env` and `node_modules`. Run it from a clean checkout, or move secrets out first. In CI, always pass `--project`, or it creates a new Project named after the checkout.
- `ployz get SERVICE --json` shows every Setting of a Service, including unset ones with their defaults.
- `ployz set SERVICE.SETTING=VALUE` stages an edit; `ployz diff` shows what is staged and `ployz publish` saves it.
- Reach another Service by reference, so Ployz knows the two are linked: `ployz set 'web.env.API_URL=http://${{ api.PLOYZ_PRIVATE_DOMAIN }}:${{ api.PORT }}'`. Single-quote it: in double quotes the shell rejects `${{`. A typed `api.internal` comes back in `typed_addresses` with the reference to set instead.
- `ployz diff --json` carries a `version`; `ployz deploy --expect-version VERSION` deploys exactly that review or refuses with `conflict`.
- A destructive command without its confirmation fails with `confirmation_required` (a person at a terminal is asked instead), naming what goes; `details.retry` is the exact command that confirms it.
- A `publish` or `deploy` that removes something running can fail with `approval_required`: the organization asks a human first. Its `details` list what it removes under `effects` and name the approval under `approval_id`. Show the user what it removes and ask them to approve it in Ployz Cloud, then run `details.retry`, which adds `--approval ID`. Never approve it yourself. Through `ployz mcp`, the user is asked in a dialog instead; a denial comes back with their reason, so don't retry it unless they ask.

## Output and errors

- With `--json`, stdout carries one JSON object and nothing prompts. A failure is `{"error": {code, message, cause, details}}`. `cause` lists every underlying error, outermost first, and is empty when there are none; human output shows only the last, deepest one. Each of these is present only when it applies: `details.next`, the command to run next; `details.retry`, the corrected command; `details.did_you_mean`, the closest valid name; `details.valid_children`, every valid name.
- Exit codes: 0 success, 1 failure, 2 usage (a command-line mistake, a missing confirmation or an ambiguous name), 3 partial.
