# Canvas and service inspector

The Canvas draws an Environment's services, volumes and domains as nodes. Clicking a node opens the Resource inspector, where a user edits that service. Behavior: `docs/user/services/`.

## Sub-features

- Nodes: one per service, with its domain, status and attached volumes (`link "pg-data"`)
- Inspector tabs: Deployments, Variables, Logs, Settings
- Settings sections: Source (image, credentials), Networking (domains, private address), Scale (Replicas, CPU limit, Memory limit), Deploy (Start command, Pre-deploy command, Healthcheck, Restart policy), Danger (Delete service)
- Variables: "N Service Variables", Raw editor, New Variable, and per row Show value, Copy value and Variable actions; then "Ployz reference defaults"
- Find (`/`) and Create (adds a service, volume or domain)

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

## Gotchas

- Inspector textboxes commit on Enter. Fill, press Enter, then check **Details** (a new row appears) and reload (the value stays).
- Status always reads Queued · Can't reach servers, because no Server is running. Don't treat it as a bug.
- Code: `ENV/-components/CanvasInspectorOverlay.tsx`, `ENV/-components/canvas/`, `ENV/services/`.
