# Canvas and service inspector

The Canvas draws an Environment's services, volumes, configs and domains as nodes. Clicking a node opens the Resource inspector, where a user edits that service. Behavior: `docs/user/services/`.

## Sub-features

- Nodes: one per service, with its domain, status, mounted configs (`link "sentry /etc/sentry"`) and attached volumes (`link "pg-data"`). A config no service mounts is its own node.
- Config drawer: Files (a tab per file, Edit | Preview, Save), Depends on, Secrets, Mounts (Mount on a service), Size, Danger (Delete config, then Keep config)
- Inspector tabs: Deployments, Variables, Logs, Settings
- Settings sections: Source (image, credentials), Networking (domains, private address), Storage (configs, Mount a config, volumes), Scale (Replicas, CPU limit, Memory limit), Deploy (Start command, Pre-deploy command, Healthcheck, Restart policy), Danger (Delete service)
- Variables: "N Service Variables", Raw editor, New Variable, and per row Show value, Copy value and Variable actions; then "Ployz reference defaults"
- Find (`/`) and Create (adds a service, volume, config or domain)

## How to get to it (user POV)

Projects → shop → the production canvas opens. Click a service node; the inspector opens on Settings.

## Driving it with agent-browser

```bash
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/production
agent-browser --session $S find text api click        # → .../services/<id>?tab=settings
agent-browser --session $S snapshot -i -c             # region "Resource inspector", textbox "Replicas" …
agent-browser --session $S find role tab click --name Variables
```

- Node links read `"api Queued Can't reach servers"`; `find text <service>` is the stable way in.
- Close with `button "Close inspector and return to Canvas"`.
- The file editor is CodeMirror, `textbox "<file> contents"`. Click `.cm-content`, then `type '.cm-content' '...'`; completions are `.cm-tooltip-autocomplete li`. Hover tooltips ignore `agent-browser mouse move`; dispatch a `mousemove` on `.cm-config-unknown` with `eval` instead.
- Escape closes the drawer; with unsaved edits it asks "Discard unsaved changes?" first.
- With real Servers, Deploy applies Config files as read-only mounts. Editing a deployed Config restarts its mounting Services; an unchanged redeploy keeps their containers. Without `SERVERS`, the seed cannot prove runtime behavior.

## Gotchas

- Inspector textboxes commit on Enter. Fill, press Enter, then check **Details** (a new row appears) and reload (the value stays).
- Status always reads Queued · Can't reach servers, because no Server is running. Don't treat it as a bug.
- Code: `ENV/-components/CanvasInspectorOverlay.tsx`, `ENV/-components/canvas/`, `ENV/services/`, `ENV/resources/$resourceId/-components/StoreConfigDrawer.tsx`.
