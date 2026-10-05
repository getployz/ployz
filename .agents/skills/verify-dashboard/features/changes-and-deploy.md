# Unpublished changes and Deploy

Edits to an Environment collect as unpublished changes until a user deploys them. The bottom bar shows them; **Details** lists them one row at a time; Deploy publishes them, and Deployments lists the history. Behavior: `docs/user/deploy/deployments.md`.

## Sub-features

- Bottom bar: **Details**, **Deploy next** (production) or **Deploy** (a Branch), **More change actions**
- Details dialog "Environment changes": "Not yet published", textbox "Deploy message", one group per service ("api will be updated"), a Change / Current Value / New Value table, per-row Discard, **Discard all changes**, **Deploy changes**
- Deployments panel: combobox "Service" (All services), navigation "Deployments" with rows like `#1 · Ship everything Queued · Deploys every service · by Ada Lovelace`

## How to get to it (user POV)

Make an edit on the canvas, then click **Details** in the bottom bar. Reach history from the nav **Deployments** link.

## Driving it with agent-browser

```bash
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/production
agent-browser --session $S find role button click --name Details
agent-browser --session $S snapshot -c                # dialog "Environment changes" and its table
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/production/deployments
```

The seed leaves production with api changes (Container image 1.4 → 1.5, `FEATURE_SEARCH`), so Details has rows before you edit anything.

## Gotchas

- Without `SERVERS`, **Deploy changes** gets admitted (the seed pairs a fake Server), but the Deploy stays Queued forever. Prove it by the new row in Deployments, not by a running status. With `SERVERS`, the Deploy runs on the real Servers.
- Code: `ENV/-components/canvas/BottomBar.tsx`, `ENV/-components/canvas/EnvironmentChangesReview.tsx`, `ENV/_canvas/deployments/`.
