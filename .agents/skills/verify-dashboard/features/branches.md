# Branches

A Branch is a copy of a parent Environment that holds only the services a user picked; the rest run live from the parent. A user changes it freely, then syncs it into the parent, where the changes wait as the parent's changes to deploy. Behavior: `docs/user/environments/environments.md`.

## Sub-features

- Branch canvas: the copied services, each `link "<service>, New, Not deployed"`; services read from the parent as `link "production's redis"`. Breadcrumb `link "Parent: production"` and `button "Environment: fix-api"`.
- `button "Sync · N"` opens `dialog "Make production match fix-api"` ("These become production's changes to deploy."): combobox `Place to sync into`, one region per service with a ticked checkbox per change (`checkbox "LOG_LEVEL"`, `checkbox "Start command New"`), checkbox `Close fix-api after syncing` (on by default, shown only for a Branch that isn't kept), **Cancel**, **Sync N changes**. Unticking a change shows **Never sync** on its row and counts the button down.
- After syncing, the parent's **Details** has `region "Included"` (`Actions for fix-api`) above the synced rows.
- `button "More sync actions"`: `menuitemcheckbox "Keep fix-api"`, `menuitem "Close fix-api…"`
- New branch: `/<env>/new-branch`, or the `button "Environment: production"` menu → "New branch of production". Panel: textbox `Name`, **Only what changes** / **Everything**, a `<resource>: Not included. Change` button per resource, switch `Keep it after syncing into production`; submit reads "Pick something to change" (disabled) until a resource is included.

## How to get to it (user POV)

From production, open the Environment breadcrumb and pick "New branch of production". Open the Branch, then click **Sync** to bring its changes into production.

## Driving it with agent-browser

```bash
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/fix-api
agent-browser --session $S snapshot -i               # button "Sync · 2" [ref=eN]
agent-browser --session $S click @eN                  # dialog "Make production match fix-api"
agent-browser --session $S snapshot -i               # button "Sync 2 changes" [ref=eM]
agent-browser --session $S click @eM                  # syncs, closes fix-api, lands on /cloud/ada/shop/production
```

## Gotchas

- Seeded state: fix-api copies api and web, with 2 changes (api `LOG_LEVEL`, web start command). Production moved on after the branch was made. staging (kept, api and worker) and search (a Branch of staging, synced into it then changed again) are also seeded.
- Region order in the Sync dialog changes between runs; match on the service name.
- **Sync N changes** with "Close fix-api after syncing" on deletes fix-api; reseed (`up.sh`) to get it back.
- Code: `ENV/-components/sync/` (SyncButton, SyncDialog, branch-close), `ENV/_canvas/new-branch.tsx`, `ENV/-components/new-branch/StoreNewBranchPanel.tsx`, `dashboard/src/components/shell/environment-breadcrumbs.tsx`.
