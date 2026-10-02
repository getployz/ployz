# Organization: projects and servers

The organization pages sit under `~`: the project list, Servers, Billing and Settings. Behavior: `docs/user/account/organizations.md`, `docs/user/servers/`.

## Sub-features

- Projects (`/cloud/ada/~`): heading "Projects", link "New project", textbox "Search projects", one card per project (`link "shop Services production 4 services"`)
- Servers (`/cloud/ada/~/servers`): **Add server**, plus "Forget them and start over." while the seeded pairing has no live Server
- Billing (`~/billing`, needs `BILLING=1`) and Settings (`~/settings`)

## How to get to it (user POV)

Use the nav links **Projects** and **Servers**, or the account menu.

## Driving it with agent-browser

```bash
agent-browser --session $S open 'http://localhost:<port>/cloud/ada/~'
agent-browser --session $S open 'http://localhost:<port>/cloud/ada/~/servers'
```

## Gotchas

- Quote URLs that contain `~`, or the shell expands them.
- **Add server** prints a join command for a real machine. Here, verify only the dialog. Pairing a real server takes `core/`.
- Code: `dashboard/src/routes/_protected/cloud/$organizationSlug/_org/~/`.
