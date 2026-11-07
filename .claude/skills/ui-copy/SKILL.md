---
name: ui-copy
description: Rewrite user-facing strings in the dashboard or CLI so they read like a product, not a spec sheet. Use after adding or changing any label, hint, description, button, badge, empty state, error, toast or CLI message, and before review.
---

# UI copy

The rule is `dashboard/DESIGN.md` rule 9: plain words, short sentences, and a hint line only when it changes what the user decides. Agents break it by explaining the feature. Run this over every string your diff adds or changes.

## Find the strings

```bash
git diff origin/main -U0 -- dashboard/src core/crates/ployz/src | grep '^+' | grep -E '"[A-Z][^"]{3,}"|>[A-Z][^<{]{3,}<'
```

Read each hit in its screen. Copy is judged where it sits, next to its label and control.

## Cut

For each string, ask what the user does differently after reading it. If nothing, delete it.

- **Narrating the machinery.** "Ployz adds one record and never touches the apex." "Opens a tunnel on every server, then closes ports 80 and 443." The user chose an outcome. Don't describe how Ployz gets there.
- **Restating the label.** A switch labelled "Cloudflare Tunnel" doesn't need "Use Cloudflare Tunnel instead of public ports".
- **Explaining the obvious consequence.** A disabled button doesn't need "Finish setup first." The open panel already says it.
- **Reassurance paragraphs.** One clause at most ("Used once, never stored."), and only where trust decides whether the user continues.
- **Listing contents in a label.** "Advanced: cluster name, Cloudflare Tunnel" is "Advanced".
- **Process and approval chatter.** "Waiting on an owner or admin. It also shows in the Agent sidebar." Show the state, not the workflow.

## Shape what's left

- A label names the thing ("Traffic", "Domain", "API token"). A button starts with a verb, in sentence case ("Switch", "Create tunnel", "Rotate key").
- A hint is one short sentence, or a concrete example in mono (`db.prod.acme.ployz.internal`).
- Show values, not prose about values. `web.ada.ployz.test → web-production.acme-apps.com` beats "Moves web.ada.ployz.test to web-production.acme-apps.com in shop/production".
- A copyable thing is a copy block, not a sentence that contains it.
- An error says what is valid: "Keys start with tskey-auth-.", not "That isn't a Tailscale auth key."
- Use the terms in `dashboard/CONTEXT.md` or `core/CONTEXT.md`. Never internal names (ZFS, Inngest, container IDs, Corrosion).

## Before and after

These come from the cluster ingress prototype, which was rewritten with this skill.

| Before | After |
|---|---|
| Advanced: cluster name, Cloudflare Tunnel | Advanced |
| Use Cloudflare Tunnel instead of public ports. Servers open no ports. Visitors arrive through your own Cloudflare zone. | Cloudflare Tunnel. Serve through Cloudflare with no open ports. |
| Needs Zone DNS Edit and Cloudflare Tunnel Edit. Ployz uses it once to create the tunnel and never stores it. | Needs DNS and Tunnel edit access. Used once, never stored. |
| Apps get names like web-production.acme-apps.com. Ployz adds one record, *.acme-apps.com. It never touches acme-apps.com itself. | Apps get `*.acme-apps.com` addresses. |
| Tunnel ready on acme-apps.com. Servers join with ports 80 and 443 closed. The token is gone. | Tunnel ready. `acme-apps.com` |
| How visitors reach your apps. Every server uses the same way in. | Traffic |
| Servers answer on ports 80 and 443. Ployz gets their certificates. | Ports 80 and 443 open on every server. |
| Opening the tunnel. 1 of 2 servers connected. Ports 80 and 443 close once every server is. | Connecting servers… |
| Review what changes before it goes ahead. | These addresses will change. |
| Moves web.ada.ployz.test to web-production.acme-apps.com · shop/production | `web.ada.ployz.test → web-production.acme-apps.com` |
| Old addresses stop answering. Update links and webhooks that use them. | Old addresses stop working. |
| Reach every port on your cluster from your tailnet: Postgres, Redis, UDP. | Private access to your servers. |
| Make it reusable and tagged, for example tag:ployz. It is sealed to your cluster, so Ployz Cloud can't read it. | Use a reusable, tagged key. |
| That isn't a Tailscale auth key. It starts with tskey-auth-. | Keys start with tskey-auth-. |
| macOS, Windows, iOS and Android accept routes on their own. On Linux, run: … Then connect to … For names to resolve, add … as a split DNS domain. | **Linux** with a copy block, then **Split DNS** with a copy block |
| Page whoever is on call when Monitoring sees a service go down. | Page on-call when a service goes down. |

## Check

Read the screen top to bottom out loud. Each sentence you'd skip as a user goes. Then screenshot it through the `verify-dashboard` skill and look again. Copy that reads fine in a diff often stacks into a wall on the page.
