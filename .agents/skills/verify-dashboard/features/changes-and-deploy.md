# Unpublished changes and Deploy

Edits to an Environment collect as unpublished changes until a user deploys them. The bottom bar shows them; **Details** lists them one row at a time; Deploy publishes them, and Deployments lists the history. Behavior: `docs/user/deploy/deployments.md`.

## Sub-features

- Bottom bar (`group "Bottom bar"`, shown only while there are changes): a count ("2 changes"), **Details**, the deploy button, **More change actions** (Save only, Deploy, keep other services, Discard all changes). The deploy button reads **Deploy next** while a Deploy is queued or running (production, because of the seeded Deploy), **Deploy** otherwise, **Request deploy** in a Protected Environment for someone who can't protect it, and **Add servers** when the organization has none.
- Details dialog "Environment changes": "N changes in production, not yet saved.", textbox `Change message` (placeholder "Message (optional)"), `region "Changes"` with one row per change (`<service> <setting> before → after`) and an `Actions for <service> <setting>` menu holding **Discard**; a `region "Included"` list when a Branch was synced in; footer **Discard all**, **Save only**, **Deploy changes**
- Deploy changes opens the new Deployment's page (`heading "#N · <message>"`)
- Deployments page: combobox `Service` (All services), `navigation "Deployments"` with rows like `link "#1 · Ship everything Queued · Deploys every service · by Ada Lovelace"`. A newer Deploy marks the older one Superseded.

## How to get to it (user POV)

Make an edit on the canvas, then click **Details** in the bottom bar. Reach history from the nav **Deployments** link.

## Driving it with agent-browser

```bash
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/production
agent-browser --session $S find role button click --name Details
agent-browser --session $S snapshot -i -c             # dialog "Environment changes", buttons "Actions for api Source" …
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/production/deployments
```

The seed leaves production with 2 api changes (Source traefik/whoami v1.10.3 → v1.11.0, `FEATURE_SEARCH`), so Details has rows before you edit anything.

## Gotchas

- Without `SERVERS`, **Deploy changes** gets admitted (the seed pairs a fake Server), but the Deploy stays Queued forever. Prove it by the new row in Deployments, not by a running status. With `SERVERS`, the Deploy runs on the real Servers, and the seed skips the queued Deploy, so the button reads **Deploy**.
- production is Protected, as every new project's default Environment is. Ada and Grace deploy; Linus (a member) gets **Request deploy**.
- Code: `ENV/-components/canvas/BottomBar.tsx`, `ENV/-components/canvas/EnvironmentChangesReview.tsx`, `ENV/-components/canvas/useStoreChangeActions.tsx`, `ENV/deployments/`, `ENV/-components/DeploymentsList.tsx`.
