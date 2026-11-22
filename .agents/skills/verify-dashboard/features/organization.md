# Organization: projects, servers and settings

The organization pages sit under `~`: Projects, Servers, Organization (settings, integrations, billing) and the per-user Account. Behavior: `docs/user/account/`, `docs/user/servers/`.

## Sub-features

- Projects (`/cloud/ada/~`): heading "Projects", link **New project**, textbox `Search projects`, one card link per project (`link "shop Services production 5 services"`, `link "blog Services production 1 service"`). A search with no match shows "No matching projects".
- Servers (`/cloud/ada/~/servers`): **Tokens** (`~/servers/tokens`), **Add servers** (opens `dialog "New fleet token"`). While the seeded pairing has no live Server: "Can't reach your servers right now" and **Forget them and start over.**
- Organization (`~/settings`, title "Organization"): rail links General, Network, Members, Tokens, GitHub, Builds (`?section=<name>`), Integrations (`~/integrations`: Tailscale, Cloudflare, GitHub). Members has **Invite**; Tokens has **New token**.
- Billing (`~/billing`): needs `BILLING=1`; without it the page is a not-found with **Go home**.
- Account (`~/account`): Profile, Sessions, Notifications, Passkeys
- `button "Open account menu"`: Account, Switch organization, theme radios System / Light / Dark, Home, Log out

## How to get to it (user POV)

Use the nav links **Projects**, **Servers** and **Organization**, or the account menu.

## Driving it with agent-browser

```bash
agent-browser --session $S open 'http://localhost:<port>/cloud/ada/~'
agent-browser --session $S open 'http://localhost:<port>/cloud/ada/~/servers'
agent-browser --session $S open 'http://localhost:<port>/cloud/ada/~/settings?section=members'
```

## Gotchas

- Quote URLs that contain `~`, or the shell expands them.
- **Add servers** mints a fleet token for a real machine. Here, verify only the dialog. Pairing a real server takes `SERVERS=1` or `core/`.
- Code: `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/`, `_org/-components/` (members, tokens, account menu), `dashboard/src/components/shell/dashboard-navigation-model.ts`.
