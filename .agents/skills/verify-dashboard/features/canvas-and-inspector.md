# Canvas and service inspector

The Canvas draws an Environment's services, volumes and configs as nodes. Clicking a node opens the Resource inspector, where a user edits that resource. Behavior: `docs/user/services/`.

## Sub-features

- Nodes: one link per service, named with commas: name, public domain if any, deploy chip, staged chip, status, issues (`link "api, Queued, 2 changes, Can't reach servers, Unreachable, 1 issue"`, `link "web, acme.com, Queued, Can't reach servers"`). An issue is its own link (`link "Unreachable, 1 issue"`) and opens Monitoring. Mounted configs (`link "sentry /etc/sentry"`) and attached volumes (`link "pg-data"`) hang under their service. A config no service mounts is its own node.
- Inspector header: the name (`heading "Edit service name"` behind it), **Fill canvas**, `link "Close inspector"`
- Service tabs: Deployments, Variables, Logs, Monitoring, Settings (Settings by default; the URL carries no `tab` until you pick one)
- Settings sections, empty ones hidden: Source (`Change container image`, `Source actions`, `Add credentials`), Networking (`Generate domain`, `Custom domain`, textbox `Private address`), Storage (only a database without a volume), Scale (a Replicas row: combobox `Region of row 1`, `One fewer` / `One more`; **Resource limits** reveals textboxes `CPU limit` and `Memory limit`, and becomes **Remove resource limits**), Build (Git-source services only, so not in the seed), Deploy (textbox `Start command`, **Pre-deploy command**, **Healthcheck**, combobox `Restart policy`, combobox `Update order`), Danger (**Delete service**)
- Variables: heading "N Service Variables", **Raw editor**, **New Variable**, and per row `Show KEY` (then `Hide KEY`), `Copy KEY`, `Actions for KEY`. A sealed row has `button "Sealed"` instead of Show and Copy. Then "N Ployz reference defaults".
- Config drawer, one page with no tabs: Files (`tablist "Files"` with a tab per file, `<file> options` showing size and permissions, **Add file**, Edit | Preview, Save), Mounts (`Actions for <service> mount` → Edit directory, Unmount; **Mount on a service**), Danger (**Delete config**, then Keep config)
- Volume drawer: tabs **Copies** and **Settings**. Settings has Space (textbox `Storage limit`, switches `Use a plain Docker volume (not recommended)` and `Allow shared writes`), Mounts (same controls as a config), Danger (**Delete volume**)
- **Find** (`/`) opens `dialog "Find a resource"` listing services, volumes and configs. **Create** lists GitHub repository, Docker image, Database, Empty service, Volume, Config. Domains are added under a service's Networking, not from Create.

## How to get to it (user POV)

Projects → shop → the production canvas opens. Click a service node; the inspector opens on Settings.

## Driving it with agent-browser

```bash
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/production
agent-browser --session $S find text api click        # → .../services/<id>
agent-browser --session $S snapshot -i -c             # region "Resource inspector", tab "Settings" …
agent-browser --session $S find role tab click --name Variables
agent-browser --session $S click @eN                  # link "Close inspector" (or press Escape)
```

- `find text <service>` is the stable way into a node. A volume or config link is covered while an inspector is open: close it first.
- Close with `link "Close inspector"` by ref, or `press Escape`. `button "Close inspector and return to Canvas"` is the shade behind the drawer; clicking it reports "covered by".
- Click **Find** by ref. `find role button --name` misses it (its name is "Find /"), and `press /` from agent-browser did not open it.
- The file editor is CodeMirror, `textbox "<file> contents"`. Click `.cm-content`, then `type '.cm-content' '...'`; completions are `.cm-tooltip-autocomplete li`. Hover tooltips ignore `agent-browser mouse move`; dispatch a `mousemove` on `.cm-config-unknown` with `eval` instead.
- Escape closes the drawer; with unsaved edits it asks "Discard unsaved changes?" first.
- With real Servers, Deploy applies Config files as read-only mounts. Editing a deployed Config restarts its mounting Services; an unchanged redeploy keeps their containers. Without `SERVERS`, the seed cannot prove runtime behavior.

## Gotchas

- Inspector textboxes commit on Enter or when focus leaves them; a registry token commits only on Enter or its ✓, so its Cancel discards it. Fill, press Enter or Tab, then check **Details** (a new row appears) and reload (the value stays).
- After a reload, wait until the page hydrates before typing: `eval 'Object.keys(document.body.firstElementChild||{}).some(k=>k.startsWith("__react"))'` reads `true`. Typing into the server-rendered page saves nothing.
- Without `SERVERS`, nodes read "Checking" for the first seconds, then "Can't reach servers" with the "Queued" chip from the seeded Deploy. Don't treat it as a bug. The Unreachable and OOM issues come from the uptime and metrics fixtures.
- Code: `ENV/-components/CanvasInspectorOverlay.tsx`, `ENV/-components/canvas/` (`node-status.ts` builds node names), `ENV/_canvas/services/$serviceId/-components/StoreServiceDrawer.tsx`, `ENV/_canvas/resources/$resourceId/-components/StoreConfigDrawer.tsx`, `StoreVolumeDrawer.tsx`, `_project/-components/service-create/create-menu-items.ts`.
