---
name: ui-copy
description: Rewrite user-facing strings in the dashboard, CLI and engine so they read like a product, not a spec sheet. Use after adding or changing any label, hint, button, badge, empty state, toast, error or refusal, CLI output or help, `ployz status` line, Approval card or prompt, published-command description, or `docs/user` page, and before review.
---

# UI copy

The rule is `dashboard/DESIGN.md` rule 9: plain words, short sentences, and a hint line only when it changes what the user decides. For CLI and engine text, the CLI output section of `core/CODING_STANDARDS.md` also holds. Agents break the rule by explaining the feature. Run this over every string your diff adds or changes, on every surface: the dashboard, CLI output and help, `ployz status`, refusals and errors, Approval cards and their terminal prompts, published-command descriptions that agents read, and `docs/user`.

## Find the strings

```bash
git diff origin/main -U0 -- dashboard/src core/crates ':!*tests.rs' ':!*/tests/*' ':!*.test.*' \
  | grep -E '^\+[^+]' | grep -vE '^\+\s*(//|/\*|\*|#\[)' \
  | grep -E '"[A-Z][^"]{2,}"|>[A-Z][^<{]{2,}<|["`][^"`]*[[:alpha:]]+ [[:alpha:]]+|^\+\s*[[:alpha:]][^"`]* [[:alpha:]][^"`]*["`][,;)]*$|^\+\s*([^"`;=(){}<>]|\{[[:alnum:]_.]+\})*[[:alpha:]]+ [[:alpha:]]+([^"`;=(){}<>]|\{[[:alnum:]_.]+\})*$'
```

The last grep's alternatives, in order, catch:

1. A capitalized single-word string, such as `"Network"`.
2. JSX text between tags on one line, such as `>Save<`.
3. Any string of two or more words, whatever its first character, so `format!("{} is not advertised")`, `"--cluster-name is ignored…"` and template literals count. It also catches the first line of a multi-line string.
4. The closing line of a multi-line string.
5. A line of bare words: JSX text on its own line, such as `Applies when a server is online.`, or the middle of a multi-line string.

The command skips tests and comments, and still returns some SQL and panic text. Skip those by eye. Read the `docs/user` diff whole, since all of it is copy.

Read each hit where the user meets it. Copy is judged in place, next to its label and control, or in the real terminal output.

## Cut

For each string, ask what the user does differently after reading it. If nothing, delete it. Keep every value the user needs to act, such as a permission name, an example, a count, or the project and environment.

- **Narrating the machinery.** "Ployz adds one record and never touches the apex." "Opens a tunnel on every server, then closes ports 80 and 443." The user chose an outcome. Don't describe how Ployz gets there.
- **Restating the label.** A switch labelled "Cloudflare Tunnel" doesn't need "Use Cloudflare Tunnel instead of public ports".
- **Explaining the obvious consequence.** A disabled button doesn't need "Finish setup first." The open panel already says it.
- **Reassurance paragraphs.** One clause at most ("Used once, never stored."), and only where trust decides whether the user continues.
- **Listing contents in a label.** "Advanced: cluster name, Cloudflare Tunnel" is "Advanced".
- **Process and approval chatter.** "Waiting on an owner or admin. It also shows in the Agent sidebar." becomes "Waiting on an owner or admin." Show the state and who acts next, not the workflow.

## Shape what's left

- A label names the thing ("Traffic", "Domain", "API token"). A button starts with a verb, in sentence case ("Switch", "Create tunnel", "Rotate key").
- A hint is one short sentence, or a concrete example in mono (`db.prod.acme.ployz.internal`).
- Show values, not prose about values. `shop/production` · `web.ada.ployz.test → web-production.acme-apps.com` beats "Moves web.ada.ployz.test to web-production.acme-apps.com in shop/production".
- A copyable thing is a copy block, not a sentence that contains it.
- An error says what is valid: "Keys start with tskey-auth-.", not "That isn't a Tailscale auth key."
- Use the terms in `dashboard/CONTEXT.md` or `core/CONTEXT.md`. Never internal names (ZFS, Inngest, container IDs, Corrosion).

## Keep it consistent

- **One casing per term per surface.** Take the term from the CONTEXT.md that owns the surface: `core/CONTEXT.md` for CLI and engine text, `dashboard/CONTEXT.md` for the dashboard. Match the casing the surface's existing strings use, so a new string never puts "the Cluster" beside "the cluster".
- **One rule, one wording.** When the engine and the dashboard enforce the same rule, both say it in the same words. A Cluster Name check reads "Use lowercase letters, numbers and hyphens." in both places, not that in the dashboard and "one DNS label: up to 63 lowercase letters, digits and '-'" from the engine.
- **A success line is true when it prints.** Report only what has happened. If the change is saved but waits on a Server, say "Saved. Applies when a server is online.", not "Named the Cluster beta."

## Before and after

These come from the cluster ingress prototype, which was rewritten with this skill.

| Before | After |
|---|---|
| Advanced: cluster name, Cloudflare Tunnel | Advanced |
| Use Cloudflare Tunnel instead of public ports. Servers open no ports. Visitors arrive through your own Cloudflare zone. | Cloudflare Tunnel. Serve through Cloudflare with no open ports. |
| Needs Zone DNS Edit and Cloudflare Tunnel Edit. Ployz uses it once to create the tunnel and never stores it. | Needs Zone DNS Edit and Cloudflare Tunnel Edit. Used once, never stored. |
| Apps get names like web-production.acme-apps.com. Ployz adds one record, *.acme-apps.com. It never touches acme-apps.com itself. | Apps get `*.acme-apps.com` addresses. |
| Tunnel ready on acme-apps.com. Servers join with ports 80 and 443 closed. The token is gone. | Tunnel ready. `acme-apps.com` |
| How visitors reach your apps. Every server uses the same way in. | Traffic |
| Servers answer on ports 80 and 443. Ployz gets their certificates. | Ports 80 and 443 open on every server. |
| Opening the tunnel. 1 of 2 servers connected. Ports 80 and 443 close once every server is. | Connecting servers… 1 of 2 |
| Review what changes before it goes ahead. | These addresses will change. |
| Moves web.ada.ployz.test to web-production.acme-apps.com · shop/production | `shop/production` · `web.ada.ployz.test → web-production.acme-apps.com` |
| Old addresses stop answering. Update links and webhooks that use them. | Old addresses stop working. |
| Reach every port on your cluster from your tailnet: Postgres, Redis, UDP. | Private access to your servers. |
| Make it reusable and tagged, for example tag:ployz. It is sealed to your cluster, so Ployz Cloud can't read it. | Use a reusable key tagged `tag:ployz`. |
| That isn't a Tailscale auth key. It starts with tskey-auth-. | Keys start with tskey-auth-. |
| macOS, Windows, iOS and Android accept routes on their own. On Linux, run: … Then connect to … For names to resolve, add … as a split DNS domain. | **Linux** with a copy block, then **Split DNS** with a copy block |
| Page whoever is on call when Monitoring sees a service go down. | Page on-call when a service goes down. |

## Check

Read the screen or the terminal output top to bottom out loud. Each sentence you'd skip as a user goes. Then look at it where it renders. For the dashboard, screenshot it through the `verify-dashboard` skill. For the CLI, run the command, its `--help` and `ployz status`, and read the real output. Copy that reads fine in a diff often stacks into a wall on the page.
