---
name: Ployz Cloud
description: A quiet operations lens for deploying and running applications on user-controlled infrastructure.
colors:
  ink: "#111111"
  ink-hover: "#2a2a2a"
  on-ink: "#ffffff"
  intent-pink: "#d0268c"
  intent-deep: "#a80068"
  intent-soft: "#fff0f7"
  intent-border: "#efb3ce"
  canvas: "#ffffff"
  surface-subtle: "#fafafa"
  surface-muted: "#f2f2f2"
  ink-muted: "#666666"
  rule: "#dedede"
  success: "#42946e"
  success-soft: "#dff1e9"
  warning: "#ad871f"
  warning-soft: "#f9efd2"
  danger: "#b62d2b"
  danger-soft: "#fbeaea"
  info: "#2057c5"
  info-soft: "#e8effc"
  dark-canvas: "#0a0a0a"
  dark-surface: "#161616"
  dark-ink: "#ededed"
  dark-rule: "#333333"
typography:
  headline:
    fontFamily: "Geist Variable, ui-sans-serif, sans-serif"
    fontSize: "20px"
    fontWeight: 600
    lineHeight: 1.4
    letterSpacing: "-0.01em"
  title:
    fontFamily: "Geist Variable, ui-sans-serif, sans-serif"
    fontSize: "16px"
    fontWeight: 500
    lineHeight: 1.375
    letterSpacing: "normal"
  body:
    fontFamily: "Geist Variable, ui-sans-serif, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.43
    letterSpacing: "normal"
  label:
    fontFamily: "Geist Variable, ui-sans-serif, sans-serif"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: 1.33
    letterSpacing: "normal"
  mono:
    fontFamily: "Geist Mono Variable, ui-monospace, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.5
    letterSpacing: "normal"
rounded:
  sm: "8px"
  md: "10px"
  lg: "12px"
  xl: "16px"
  pill: "9999px"
spacing:
  "1": "4px"
  "2": "8px"
  "3": "12px"
  "4": "16px"
  "5": "20px"
  "6": "24px"
  "8": "32px"
components:
  button-primary:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.on-ink}"
    typography: "{typography.body}"
    rounded: "{rounded.lg}"
    padding: "0 10px"
    height: "32px"
  button-outline:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.lg}"
    padding: "0 10px"
    height: "32px"
  input-default:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.lg}"
    padding: "4px 10px"
    height: "32px"
  input-staged:
    backgroundColor: "{colors.intent-soft}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.lg}"
    padding: "4px 10px"
    height: "32px"
  card:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.xl}"
    padding: "16px"
  nav-item-active:
    backgroundColor: "{colors.surface-muted}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.md}"
    padding: "8px"
    height: "32px"
---

# Design System: Ployz Cloud

## Overview

**Creative North Star: "The Quiet Operations Lens"**

Ployz Cloud is a calm, precise lens over product context and core runtime truth. The interface stays visually quiet during normal operation, makes valid paths feel inevitable, and raises only the information that changes a decision. It never pretends Cloud is runtime authority: core testimony and bounded operations remain legible, including honest uncertainty when evidence is missing or stale.

This design system governs the authenticated product dashboard. The marketing site (served at `/`, `/home` and every path the app has no route for, proxied from `MARKETING_ORIGIN`) is a separate brand surface, built outside this app, and must not drive product density, component behavior, or visual hierarchy. Product UI is restrained rather than theatrical: familiar controls, compact information, progressive disclosure, and state transitions that explain what just changed.

This register defines the target state for the ongoing dashboard cleanup. Existing tokens and components may still use the previous accent until they are migrated; new and revised product UI should follow this system.

The system explicitly rejects verbose infrastructure administration, self-hosting dashboards that expose their implementation complexity, generic pages dominated by forms, and recognizable AI-generated design patterns. It refines the existing component foundation instead of decorating over inconsistency.

**Key Characteristics:**

- Nearly achromatic at rest, with warm pink reserved for staged intent.
- Compact Geist typography and familiar controls built for repeated daily use.
- Runtime evidence and authored product context presented without blurring authority.
- Autosaved changes remain visible from edited field through diff and deployment.
- Tonal layering and borders establish structure; shadows indicate real elevation.

## Colors

The product palette is neutral first. Ink and white carry action hierarchy; warm pink is the only chromatic identity signal and means staged user intent, never health or failure.

### Primary

- **Action Ink** (`#111111`): primary buttons, high-emphasis text, and decisive controls on light surfaces. It reverses to white in dark mode.
- **Action Hover** (`#2a2a2a`): the restrained hover state for ink actions; never a decorative gray panel.

### Secondary

- **Intent Pink** (`#d0268c`): the saturated anchor for staged intent and focus. It is deliberately warm and unmistakably pink, never violet or purple.
- **Intent Deep** (`#a80068`): accessible intent text and compact indicators on pale staged surfaces.
- **Intent Soft** (`#fff0f7`) and **Intent Border** (`#efb3ce`): the background and boundary applied to autosaved fields and diff rows that differ from deployed truth.

### Tertiary

- **Evidence Green** (`#42946e`): successful or healthy runtime evidence. It also marks a canvas node the next Deploy creates.
- **Attention Amber** (`#ad871f`): warnings and consequences that require consideration.
- **Failure Red** (`#b62d2b`): errors, invalid state, and destructive intent.
- **Information Blue** (`#2057c5`): neutral informational state and links where surrounding context does not already establish interactivity. It also marks a canvas node the next Deploy changes, and a Deploy in flight.
- Each semantic hue has a pale companion surface. Text, iconography, and state language must accompany the color.

### Neutral

- **Clear Canvas** (`#ffffff`): the default page and component surface.
- **Quiet Surface** (`#fafafa`) and **Muted Surface** (`#f2f2f2`): secondary structure, selected navigation, toolbars, and disabled regions.
- **Muted Ink** (`#666666`): supporting copy that still meets contrast requirements.
- **Structural Rule** (`#dedede`): borders and dividers that clarify grouping without becoming decoration.
- Dark mode uses neutral black surfaces (`#0a0a0a`, `#161616`) and neutral light ink (`#ededed`); it must not reintroduce a violet cast.

**The Neutral Action Rule.** Ordinary primary actions are ink on light surfaces and white on dark surfaces. Chromatic fills never become a generic importance shortcut. The one exception is the bottom bar's **Deploy**, solid Intent Pink: it is where a staged change's pink trail ends.

**The Visible Intent Rule.** Pink follows an autosaved change from its field through the diff to the apply surface; on the canvas a node takes the staged colour of what the Deploy does to it. Saturated pink is rare; most staged state uses the soft surface, border, and deep text.

**The Semantic Honesty Rule.** Pink never means success, warning, failure, runtime drift, or informational status. No semantic state relies on color alone.

## Typography

**Display Font:** Geist Variable with a system sans-serif fallback
**Body Font:** Geist Variable with a system sans-serif fallback
**Label/Mono Font:** Geist Mono Variable with a system monospace fallback

**Character:** One precise sans family keeps the product coherent and lets hierarchy come from weight, spacing, and placement rather than decorative type pairing. Monospace is reserved for identifiers, commands, logs, hashes, measurements, and evidence that benefits from fixed-width scanning.

### Hierarchy

- **Headline** (600, 20px, 1.4): rare page or major panel headings.
- **Title** (500, 16px, 1.375): dialog titles, section headings, and resource names.
- **Body** (400, 14px, 1.43): controls, descriptions, tables, and routine interface copy; prose remains within 65–75 characters.
- **Label** (500, 12px, 1.33): metadata, badges, compact navigation context, and short state labels.
- **Mono** (400, 12px, 1.5): runtime evidence, identifiers, shell commands, logs, and tabular technical values.

**The Product Scale Rule.** Dashboard typography stays between 12px and 20px for routine UI. Large display typography, fluid type scales, and marketing-style headings are prohibited inside product workflows.

**The Evidence Type Rule.** Use monospace because the value is operational evidence, not because the interface should look technical.

## Elevation

Ployz uses structured flatness. Static hierarchy comes from surface tone, one-pixel rules, and spacing. Shadows are reserved for elements that genuinely move above the document—menus, popovers, dialogs, sheets, and contextual overlays. A static card does not earn a shadow merely by being a card.

### Shadow Vocabulary

- **Floating Low** (`0 1px 3px rgb(0 0 0 / 0.10), 0 2px 4px -1px rgb(0 0 0 / 0.10)`): menus, compact popovers, and lifted controls.
- **Floating Medium** (`0 1px 3px rgb(0 0 0 / 0.10), 0 4px 6px -1px rgb(0 0 0 / 0.10)`): dialogs, sheets, and contextual overlays.

**The Structured Flatness Rule.** Surfaces are flat at rest. If an element does not overlap or move independently of its surroundings, use tone, border, or spacing instead of shadow.

## Components

Components are compact, familiar, and decisive. The stock component vocabulary is the starting point; variants exist to express real state, not to add personality. Every interactive component includes default, hover, focus, active, disabled, loading, invalid, and staged states where those states apply.

### Buttons

- **Shape:** compact controls with gently curved corners (12px) and a 32px default height.
- **Primary:** Action Ink with white text and 10px horizontal padding. In dark mode, invert the relationship.
- **Hover / Focus:** move only through the ink ramp; focus adds a visible Intent Pink ring without changing layout.
- **Secondary / Ghost:** use borders or muted hover surfaces. Destructive actions use Failure Red only when the action itself is destructive.

### Chips

- **Style:** fully rounded only because chips are compact labels. Use a semantic pale surface, matching border, and short text.
- **State:** badges label state; buttons and pills perform actions. Never make a static badge behave like a control.

### Cards / Containers

- **Corner Style:** gently curved (16px) for true grouped resources; avoid nesting cards.
- **Background:** Clear Canvas at rest, semantic pale surfaces only when the entire container shares that state.
- **Shadow Strategy:** flat by default; use a one-pixel structural ring. Floating containers follow the Elevation section.
- **Internal Padding:** 16px by default, 12px for compact variants, and 24px only for focused resource nodes or dialogs.

### Canvas nodes

A node answers two questions in two places, and neither ever stands in for the other.

- **Card:** the icon, the name, and the public domain when there is one: its first custom domain, else its generated one, muted until the Deploy that adds it lands. Source, image, replicas, ports and reasons live in its panel.
- **Status line: what runs now.** One word from runtime evidence: Online, Degraded, "Crashed 2 min ago", Not running, Not deployed, No source, a grey Starting while its containers run but none serves yet, or a grey "Deployed" while a Server is missing from the evidence. Staged or in-flight work never replaces it. It ends in **⚠ N** when there is something to fix, red if any of them is a crash and amber otherwise, and the card opens its panel. A replica that passed its healthcheck and fails it now still serves, so its service stays Online and adds one amber ⚠, however many replicas fail; the ⚠ is the whole signal, and the panel adds nothing above its tabs. A service that should run and doesn't, Crashed or Not running, also turns the border red.
- **Uptime adds to ⚠, never to the word.** Every service with a public domain gets one uptime check in every Environment, and each Deploy keeps it in step with the domain and healthcheck path; there are no settings. Without an HTTP healthcheck path any answer below 500 passes, since a 4xx proves the server answers; with one, an answer must be below 400. A check is Up or Unreachable, and Unreachable adds one red ⚠ that opens the Monitoring tab. The status word stays what runtime evidence says, so a service can read Online ⚠ 1 while its domain is unreachable. A service already Crashed or Not running adds nothing for uptime, and a domain issue on the same host is one issue, not two. When a check that never passed gets a 4xx, its healthcheck path is wrong, so the Monitoring tab shows the Healthcheck path field in place, with one line of evidence: "Your app answers /health with 404." Healthcheck and Uptime stay separate words: a healthcheck runs on the Server, an uptime check from outside. Each applied Deploy probes the checks of the services it applied, and **Check now** probes one on demand; that round is evidence like a scheduled one, so it opens or closes an incident within seconds and counts toward uptime. The Monitoring tab is a status line with **Check now**, response time and incidents. Regions surface in two places only: inside an incident, and in the Response time tooltip, one row per region for the hovered step with its median, max, where the time went and what failed. A median across regions hides the one region that is slow or failing, and the tooltip appears only when the user asks about a step, so the tab stays a status line until then. Requests show only inside an incident.
- **It never guesses.** Before the first evidence it shimmers. When the connection drops, it keeps the last word in grey with its age, "Online 2 minutes ago", and a grey word is never red, never an issue. When the Servers can't be reached it says "Can't reach servers"; with no Server at all, "Needs a server".
- **Chip: anything about Deploys.** The first that applies: "Deploying 1m 12s" or "Queued"; a green "New", a blue "N changes" or a red "Removing"; on an open Deployment Page, its Node Outcome. Otherwise nothing. A node the Deploy creates, changes or removes takes the green, blue or red surface; nothing else fills a card.
- **Volumes are trays** under each service that mounts them: the name over a fill showing how full it is, amber from 85%. A tray speaks only when it must: green, blue or red when staged, "Removing", "N% full", an amber "shared · N writers" when more than one container writes it, and what runs or waits for Deploy, "Restore 11:42" or "Moving to web-2" staged and spinner text while it runs. A Deploy that only snapshots a Volume reads "Snapshotting" on its tray, and the Deploying chip shows only on services a step restarts. A volume several services share shows under each, marked shared, and hovering one lights them all. Only a volume nothing mounts is a node of its own.
- **Copies hang under their Volume**: a dashed line from the tray (or the lone Volume's card) to a small card per mirror and offsite copy, `data-web-2` over "Mirror · 4 minutes ago". A staged copy reads New in green or Removing in red, struck through, and a mirror a move promotes reads Primary in blue; no "Was" and no Undo here. A mirror more than an hour behind shows amber with its age. While something runs on a copy its card says so beside a spinner: Seeding, Refreshing, Uploading, Removing. A shared Volume's copies hang once, under the first service that shows it. A copy card grows its node's height, so it drags with the node and new nodes are placed clear of it. Clicking a copy opens its own drawer at `resources/<volume>~<copy>`, in the canvas and the phone list alike. Old copies and Snapshots stay in the Volume's drawer: they serve nothing on their own. Amended for volume copies: only mirrors hung here; an offsite copy is a place the data lives too, so it hangs beside them.
- **Configs are trays** too, above the volume trays: a folder icon, the name, and the directory it mounts at, right-aligned in monospace. A tray is staged green, blue or red like a volume's. A config several services mount shows under each. Only a config nothing mounts is a node of its own.
- **Selected**, while its panel is open: a two-pixel ink ring. **Keyboard focus:** an Intent Pink outline, drawn only while the user navigates by keyboard. A pointer press ends that, so a closed panel hands focus back to its node without drawing it.
- Only a node being dragged casts a shadow.
- **On phones** the canvas is a list of the same cards, compact: the name and chip on one row, the domain and status line on the next. Trays follow their cards.

### Projects

- Label the organization destination and page **Projects**. Keep search visible and use an ink **New project** action.
- Use three columns on desktop, two on tablet, and one on mobile.
- Each project card shows the Default Environment's Working State Services as centered icons on a subtle dotted surface. Reuse existing source icons; do not use canvas positions, connections, or Volumes.
- The whole card opens that Environment. Keep card controls out of the preview.
- Footer: `● production · 2/3 services online`; an empty Environment shows `production · No services`. Count a Service once when it has a running container that serves: its health is healthy, not configured, or failing (it passed its healthcheck since it started, so it still takes traffic). Exclude hooks. When runtime evidence is disconnected or incomplete, show only the service count.

### Settings panels

- **The header is the name and nothing else.** No status, summary, count or warning line under it, and no banner between it and the tabs, on any drawer: service, volume, config, live node, or any added later. Status lives on the canvas card; trouble lives in the tab or row where it's fixed. Reach for this space and the answer is no. One exception: a service drawer's header carries its run actions (Start or Restart, and the ⋮ menu) and, while one runs, its progress. A run that ends badly shows one alert between the header and the tabs until the next run. These act on the whole service, so no tab or row owns them.
- A row shows the value in effect. Unset shows what that means (the default, or "Image default"), never an invented example. A staged row or chip wears its change kind and nothing more: pink for a changed value, green for something new, red and struck through for something removed, the same colours Details uses. It never says what it replaces ("Was 1") and never carries Undo. Details owns that: it shows what changes from what, and Discard drops one change or all of them. The panel and Details tell one story.
- A missing value that breaks something is a row warning in Failure Red, where it's fixed: a database with no volume.
- A volume's writers are every replica of every service that mounts it, staged values included. More than one is one amber warning at the top of the volume's Mounts, with a replica tag on the mount it comes from and "Use 1 replica" when one service owns all writers. It warns only where a volume allows shared writes, or where writers predate the rule; everywhere else the panel prevents it: Replicas holds at 1 while such a volume is attached ("Limited to 1 while data is attached", with Allow shared writes beside it), and the mount picker greys a service that would be a second writer ("Already used by postgres").
- Order follows the kind. A service made from a database template leads with its private address; other services lead with their public domains. A volume leads with its mounts, then its storage, then its Copies once deployed with managed storage. Copies shows each Server, available Volume Runs, progress and recent activity. While a run is going, nothing else starts. Config and volume panels own mount management. Service Settings shows Storage only to warn when a database has no volume.
- Every resource panel (service, volume, and any added later) lays out its settings the same way: flat sections, a hairline between them, a Title-scale heading, and a line of description only where the heading isn't enough. No cards inside settings.
- A setting is a row: label and short hint on the left, its control on the right where the panel is wide; stacked on narrow panels and phones.
- An optional value is an input with a placeholder, blank while unset. Only a setting most resources never need (pre-deploy command, root directory) hides behind a small link.
- Danger comes last as plain rows: what it does, what follows in muted text, and a soft destructive button. Never red text on a red surface.
- Panel headers are the same shape: the name, renameable, without a kind label, and no second line.
- Settings pages (Environment, Project, Organization's General, Members and Builds, a Server) use the same sections, rows and danger rows as the panels. There is one Danger section per page, last.
- Settings autosave, except a name: renaming changes addresses and URLs, so it waits for an explicit Rename (a project's Rename button, a service's or volume's pencil and its dialog). Everything else saves as it's set, staged where it deploys.
- Buttons and titles use sentence case: "Forget servers", "Generate domain".

**Amendment: configured features show disabled (2026-10-14).** Never hide a feature an operator can turn on with configuration. Disable it and say why. Billing is the exception and stays hidden on Self-hosted Cloud, because only Ployz-hosted Cloud can ever have it. Account → Notifications is the first: with no Mailer its rows show each type's choice, can't be changed, and the section reads "Email isn't set up on this Cloud" with a link to the self-hosting email setup.

### Inputs / Fields

- **Style:** 32px controls, 12px corners, transparent or canvas background, one-pixel Structural Rule border, and 10px horizontal padding.
- **Focus:** visible three-pixel Intent Pink ring plus a stronger boundary; focus never depends on subtle color shift alone.
- **Staged:** an autosaved value that differs from deployed truth uses Intent Soft, Intent Border, and a restrained pink ring. The treatment persists after blur.
- **Error / Disabled:** invalid state replaces staged emphasis with Failure Red; disabled fields use a muted surface and reduced emphasis without becoming unreadable.

### Navigation

- An Environment has four places: **Architecture** (its canvas), **Deployments**, **Logs** and **Settings**. The organization has three: **Projects**, **Servers** and **Organization**, its own settings. No two places share a name, so Settings always means an Environment's.
- On desktop a rail holds the logo, the scope's places, each an icon beside its label, and the account at the bottom: your avatar, name and organization. On an Environment, Projects and Servers follow its places below a divider. Wherever the canvas shows, the rail narrows to its icons and the avatar, each naming itself on hover, and the canvas gets the room. The current place uses a muted neutral surface, never the staged-intent color.
- A page with sections lists them under its place in the rail while it's current: Settings has **Environment** and **Project**; Organization has **General**, **Members**, **Builds**, and **Billing** where billing exists. A page never repeats its sections as tabs.
- On phones the scope's places fill a bottom tab bar, and the current place's sections sit in a strip under the top bar.
- The logo tops the rail and opens Projects. The account menu holds Account, organization switching, Theme, Home (the marketing site's `/home`, a full page load) and Log out. Account is the user's own place, the same in every Organization: it has no rail entry, and the avatar row is its place, taking the current-place surface while Account is open.
- One top bar per page. On an Environment it says where you are with breadcrumbs, `project / environment`, plus the place's name when it isn't Architecture. On a Branch the path reads `project / parent ⑂ branch`: ⑂ marks what the Branch was made from, and the Parent's crumb opens the Parent. Each switcher crumb opens its switcher, and switching keeps the current place. On phones the bar also carries the logo and the avatar, the path keeps its last two crumbs and moves the rest into a "…" menu, and the bar never wraps. Never stack a second title row that repeats the place.
- Wherever Environments are listed, they form one tree: root Environments first, each Branch indented under its Parent and marked ⑂.
- The Environment switcher shows that tree with each Environment's notes: "default", "not deployed", "kept", and a PR Environment's "PR #N" and "Off". It holds no Branch controls: they are the Sync button's. It ends with **New branch of X**, which opens the New branch panel over the current Environment's canvas, and **Manage environments**, which opens Settings → Project.
- Settings has two sections, **Environment** and **Project**, each linkable. Project is where every Branch is seen and how Branches work is set up: the Default Environment picker, the project's Environment tree (each with its services-online summary, the switcher's notes and a Default chip, opening that Environment) with **New branch** (of the current Environment) and **New environment** (an empty root Environment) above it, PR environments, and **Delete project**. Environment holds what's about the Environment itself: its branch defaults and **Delete environment**. On a Branch it holds no Branch controls: they are the Sync button's.
- The canvas's **Find** button and the `/` key open the resource finder; `/` never fires while typing in a field.
- Icon-only controls always have an accessible name and a tooltip.

### Agent sidebar

The agent sidebar is a chat with Ployz Cloud that runs the same commands as `ployz mcp`. It sits on the right, under the top bar and beside the page, and belongs to the Organization rather than a place, so navigating never remounts it.

- **Amends the canvas-room bet.** The agent never covers the canvas or a page: covering the graph hid the very Services the agent was talking about. Open, it is a column beside the page, and the canvas narrows and refits to show the whole graph. Closed, it takes no room at all.
- **The Agent button is its only toggle.** The top bar spans the whole window, rail aside, so the button at its right end never moves when the agent opens or closes. There is no close button and no scope line: the crumbs already say where you are. The column's header names the chat and holds chat history and New chat. On phones the agent fills the screen under the top bar and the tab bar stays.
- **Open, the agent hosts the bottom bar.** The bottom bar leaves the canvas and docks in the agent column above the composer, so your staged changes and the agent's Approval sit in one place. Closed, the bar is back on the canvas. Pages without a bottom bar, like Servers, dock nothing.
- **It asks whenever a plan destroys something, and no setting turns that off.** A destructive Publish or Deploy, an Environment or Project removal, a server removal, a cleanup, a drain that loses the last copy, or a Volume Run that deletes data pauses on an **Approval card** in the thread. For the root of real data the card holds the typed field from Destructive actions. A shutdown asks only when it deletes Volume data. A Service restart, start or stop never asks. The card shows the destructive lines first, in the destructive color, the rest folded to a count. Approve and Deny sit on the card; Deny takes an optional reason that the agent hears. The card never uses Intent Pink, which stays staged intent.
- The card is drawn from Cloud's Approval, read fresh each time it shows, so a reload, a second browser or a `ployz` CLI waiting on the same Approval all see one card, and a superseded Approval says the plan changed.
- After Approve, or when nothing needed approving, the card becomes a **Run card** that follows the Deployment in the Deployments place's words and statuses and opens its Deployment Page. A run on Servers or Volumes shows as a Run card with the run's name and the state Cloud admitted it in.
- While an Approval waits, the closed Agent button reads how many wait, like "1 waiting", and shows just the count on phones.
- Its commands are the compiled agent tools plus the Cloud-published commands marked `agents: true`, read from the one registry Cloud also serves to `ployz`. A command marked `agents: false` never reaches an agent, in the sidebar or in `ployz mcp`.

**The Soon rule.** An option that isn't built yet appears only inside a flow that works, greyed out with a Soon tag. It is never a page, tab or button that does nothing.

### Branches

- **The top moves changes between Environments; the bottom bar stays in this one.** A Sync puts one Environment's changes into another, where they land as ordinary changes to deploy in the receiver's bottom bar, where the user lands. So it never sits in a bottom bar.
- **One button, one dialog.** Acting on this Branch happens in its **Sync button** and the **Sync dialog** it opens; there is no Branch panel. Seeing every Branch and setting up how Branches work happens in Settings → Project. Nothing else holds a Branch control.
- **Inside a Branch, few fork words.** A Branch is an Environment that syncs with the others of its Project. Its button does only that and how the Branch ends; what each Environment keeps as its own stays out of the way.
- On a Branch the canvas's top right has the **Sync button** before Find, a split button. Its label is ⑂ and the first that applies: "Shutting down" or "Shutdown failed" (a Branch that isn't a PR Environment reads "Closing"), "Off", "Sync" with the count of changes a Sync into its Parent carries in muted figures, else "In sync". Its main half opens the Sync dialog with the Parent picked. On a PR Environment, **On merge · N** sits before it.
- Its **▾ menu** holds **Keep fix-api**, a check, with "Closes in N days" while it would close for sitting idle; for a PR Environment, **Shut down until the next push**, or **Deploy pr-142** while it's Off; and **Close fix-api…**, which confirms typed and is greyed out with the reason for the Default Environment or a Branch with Branches. Places are picked in the dialog, never in the menu.
- A Live Node is drawn dashed and translucent, labelled with the Environment it comes from ("production's"), and links into it are dashed, as is the line to a Volume's mirror; every other link is solid. Opening it says whose it is, which services here use it, and opens it in its own Environment.
- A Live Node that owns data carries an amber "real data" line.
- While a Branch is being picked, the panel lists each of its Parent's nodes beside what the Branch gets; tapping one offers its two choices. On desktop the canvas picks too: clicking a card toggles it, separate ones are lit, Live Nodes dashed and left-out nodes faded.

### Destructive actions

Guard what can't come back, never the verb.

- **It comes back on its own**, like a PR Environment whose pull request is open: no dialog. Its next push brings it back.
- **It's staged**, like deleting a service, volume, variable, domain or mount: no dialog. The Review lists it and Discard undoes it.
- **A Branch that isn't kept**: one plain confirm that names its Own Copies, which started empty.
- **The root of real data**: an Environment, a Kept Branch, a project, an organization, a server, or a deploy that deletes outside a Branch that isn't kept. One dialog lists what goes by name with its canvas icon, and the user types where it is: `project/environment`, the project, the organization or the server. Up to four things are all named. Past that, anything new and the two biggest volumes are named, and the rest count per kind, a click from their names. Where Cloud knows what goes, the list opens at once with it, and anything the servers add is marked new; a list only the servers know, like a server's volumes, has nothing to be new against. The servers add sizes where they report them, and the button waits for them.
- **Locks are structural.** The Default Environment can't be deleted; nothing has a protection switch.
- **Words:** Branches and PR Environments close, servers are removed, and everything else is deleted. Confirmations name things in the user's words and never Ployz's machinery.
- A failure only Ployz can fix says what failed and offers Retry; it never leaves a dialog that can't go forward.

### Apply Changes

The staged-change system connects edited fields, affected resources, and environment-wide review. Every screen size places the change count, Review and Deploy in the bottom bar. Review replaces the workspace rather than stacking a dialog over a resource inspector. Save puts configuration in Saved State without deploying; Deploy saves when needed and starts deployment. These are distinct visible review actions. Returning from review restores the editor and canvas context.

History opens over the canvas and lists immutable Saved versions with available message, actor and time. The version menu offers Undo this change and Restore this version. Undo preserves unrelated later draft edits and requires a recorded predecessor. Restore stages the whole snapshot. If either action overwrites draft fields, the dialog names those fields and requires consent against the exact current review. A stale confirmation cannot overwrite a newer draft. History actions and Discard change only the draft; Save records them explicitly. Changes remains reachable for a Saved-only inverse even when no runtime changes remain. Config and Volume inspectors continue to own mounts.

### Deployments

Deployments are a place, not a mode of the canvas. Each Deployment has its own Deployment Page, which opens as a panel over the canvas; the canvas stays mounted underneath and always draws the Environment as it is now. Closing the page leaves the canvas, its selection and its viewport as they were.

- While the page is open, the canvas lights up what the attempt changed: those nodes show their Node Outcome and the rest dim. Nodes it removed, or that were deleted since, appear only on the page.
- The page's header places the attempt in two lines: its number and message, status and duration, beside the one action its status allows (the rest wait in ⋮); then who started it, the commit or upload it ships, and the age. Why it failed sits under the header, with the fix.
- A tab per changed service, marked with its outcome, picks whose logs show; past six they become a dropdown. The changed volumes follow, each marked with its outcome. The logs fill the rest of the panel, and a service the attempt never reached says so instead. **Build | Deploy** follows the running stage: Build while building, Deploy once deploying, the failed stage on failure, until the user picks one. A prebuilt image has only deploy logs, so its service shows no Build | Deploy.
- A manual Deploy opens its page when the user's "open started deployments" setting is on. Git-triggered deployments never take over the screen, and nothing returns the user to the canvas automatically. Deploying while another deployment runs queues.

One floating **bottom bar** sits at the bottom of the canvas on every screen size and stays usable while a panel is open. It holds this Environment's own changes and nothing else. It shows only while this Environment has staged changes or a running attempt, and holds nothing from a pull request and no blocked state. One row, the first that applies: its words, then its actions, at full control size, like Railway's. It sizes to its content up to 36rem; its lines truncate, and values live in Details.

1. **Changes to deploy.** The row takes the staged-intent surface: "N changes", then **Details · Deploy · ⋮** (⋮ holds Discard). "· N not set" follows when secrets on any Service have no value: the bar is where Deploy is pressed, and Deploy leaves out every Not set key, so it counts them all. Deploy is solid pink, and its tooltip says ⇧+Enter. What changed shows on the canvas and in Details, never in the bar. Deploy reads **Deploy next** while another attempt runs or waits, because deploying then queues. Git-triggered deployments never clear these changes. An Environment that has never deployed shows all its nodes here.
2. **A running or queued attempt** whose Deployment Page isn't open: its status and message, the service and step it is on, and **Logs**, which opens its page.

Nothing about a Branch sits in the bar: a Sync, a shutdown and closing are the Sync button's.

**Details** reviews the changes to deploy in the Sync dialog's anatomy: calm and flat, with colour only for the kind of change, Intent Pink for changed, success green for added and red for removed.

- **Header:** "Environment changes" and one line, "6 changes in fix-api, not yet saved." An optional message field follows whenever Save or Deploy is available.
- **Groups** say once, in words, where changes came from: **Your changes**, the edits made here, then **From production's deploy**, what Follow brought, under one line, "fix-api is a Branch of production, so what production deploys arrives here too." Changes another Environment synced here group under "From staging". A group shows only with rows, and Your changes alone has no heading, as on a root Environment.
- **Included** lists each source a Sync brought here, "staging · 1 change". Its ⋯ holds **Remove**, which takes that source's changes back out, and **Include newer changes** once the source changed since, which opens the top's Sync dialog for just what's new. These are this Environment's own changes, named by where they came from, so they belong in the bottom bar; starting a Sync still lives only at the top.
- **Rows:** one line per change: its kind's marker (pink pencil, green plus, red minus), the node's name muted, the setting (a variable's key in monospace, a Setting by its title), and the value right-aligned in small monospace, `old → new` with the new value in the kind's colour, or a removed value struck through. A node added or removed as a whole is one line in its colour, "web · will be added". No cards in cards, no table headers.
- **⋯** holds a row's actions: Discard, plus Never sync on a change that arrived from another Environment. A setting that can't be discarded alone offers "Discard all of api".
- **A Use hint** is a quiet second line under its row: "production has since set `info` · Use theirs".
- **Footer:** a quiet **Discard all** on the left; **Save**, and **Deploy changes** when a Server can run them, on the right.

The **Sync dialog** is the one review of a Sync, whichever way it goes. It is calm, flat and monochrome: ink for the primary button and the ticks, amber only for "Changed in X", and no Intent Pink, which stays for staged intent and Deploy.

- **Header:** the title "Make [production ▾] match fix-api", with the place picker in the title, and one line, "These become production's changes to deploy." With nothing to sync the line says so, and the footer offers only **Done**.
- **Rows:** a section per Service or Volume, its name small and muted with its icon. Each change is one row: a tick, its name (a variable's key in monospace, a Setting by its title), at most one badge ("Changed in production", else "Secret", else "New"), and the old → new value right-aligned in small monospace. A secret production lacks reads "Value set in production" instead: its value never syncs. A new Service's or Volume's settings follow its tick.
- **Defaults:** every change is ticked, but for a change the sender only inherited from a Parent it isn't syncing into.
- **Unticking** leaves a change out this time; the row dims and offers a small **Never sync**, which marks it in the sender and moves it to the never-synced list at once.
- **Footer:** "2 never synced ▾" on the left opens the list above the footer, each with **Sync again**. **Cancel** and "Sync 3 changes" sit on the right.
- **Close fix-api after syncing**, a check on by default, sits above the footer when a Branch that isn't kept syncs into its Parent.
- A review that went stale stays open with the fresh rows and says so.

A Sync never deploys and never waits for a deploy: it takes what the sender has, deployed or not. The user lands on the receiver's canvas with the toast "Synced 3 changes from fix-api · Undo"; Undo discards those changes there, and the next Sync offers them again.

**On merge.** A PR Environment's toolbar shows **On merge · N**, the pull request's bottom bar. It opens a panel listing every change since the preview started that goes to its Destination when the pull request merges on GitHub, one row each: the setting, then `production value → value`. Every change joins by default. **Edit** sets the value production gets instead of the preview's; a secret production lacks reads **Not set** until edited. **Exclude** keeps a row in the preview only; Excluded rows stay greyed with **Include**. A setting production changed after the preview started shows `CACHE_TTL: production 45 → 60`; the preview's value wins. A variable the preview removed rides as **Removed**. A Service or Config the preview removed reads **Removes db**, with **Changed there** when production changed it since; one production still uses reads **In use** with its users and stays while the rest lands; a removed Volume reads **Kept** and never leaves. Only nodes the preview copied can be removed. Seals, Sizing and custom domains never ride. The pull request's Ployz check mirrors the list and is green or neutral, never red. Picking production in the Sync dialog takes the changes now, as production's changes to deploy, and they leave the list. **Shut down until the next push** in the menu takes pr-142 off the Servers at any time: the button says so while it shuts down, then "Off", with **Deploy pr-142** in the menu, its services reading Off and the Environments list saying Off; a failed shutdown offers **Shut down** again.

**Amendment: a merge saves (2026-10-11, replaces "a merge never saves").** The GitHub merge is the Save. The pull request is where people review, and the check names every change before anyone merges, so a merge is no hidden Save; making someone click Apply afterwards was a second review that nobody understood. The merge writes one Saved revision in production, "Merged #146 · 3 changes", and the merge commit's push deploys it with the code. Undo in History reverts it. It isn't a Sync: a Sync still never deploys and never waits. Production shows nothing from a pull request before it merges, and nothing ever blocks Save or Deploy: a secret without a value is skipped and shown Not set. A merge that removes a Service opens no Approval: it retires the Service's containers and never authorizes deleting Volume data, so nothing it does is the destructive kind an Approval guards. Undo brings the node back whole.

Bar text stays minimal: fewer words on mobile, and explanations belong in a panel, never in the bar.

**The One Vocabulary Rule.** A state looks and behaves the same in every field, resource, drawer, diff row, and toolbar. Local reinvention is a defect.

### Volume storage

- New Volumes default to managed storage with a limit; Docker storage is an explicit Advanced choice, never a silent fallback when no compatible Server is available. Such a Volume stays staged and says it needs a compatible Server. Unknown or incomplete runtime evidence does not establish that managed storage is unavailable.
- Storage settings are editable until deployment is requested, then shown as fixed, including after failed or cancelled attempts.
- New Volumes refuse a second writer unless shared writes is on: one Service with one replica. Allow shared writes, under the volume's Advanced, applies at once and is never staged; turning it off with several writers is refused, and the reason shows on the switch.
- Product copy does not expose ZFS or imply backups, replication, or resizing.
  - Exception: the Add Server dialog may name ZFS where it explains a requirement, because "managed volumes" alone doesn't tell someone why they'd opt out.
  - Exception: a Volume's copies are places. Its mirror is a read-only copy on another Server, refreshed on every Deploy and every 15 minutes, named `data-<server>`, never a replica. Its offsite copy lives in an S3 bucket and uploads daily; its points may be called Backups. A move leaves an old copy on the Server it left. Snapshots are points on the live Server, shown only in the drawer. Offsite copies and Snapshots show only where the Servers serve them.
- A Volume's drawer has a Copies view in two groups. Live holds the primary Server, a staged restore and the Snapshots. Elsewhere holds the mirror, the offsite copy and old copies. Add copy shows only while a mirror or an offsite copy is still missing.
- What changes where data lives waits for Deploy: Restore, Add copy, Remove copy and Make primary. They count in the bottom bar and Details, and Deploy runs them before its Services, so one Deploy is the only thing that moves data. A copies-only Deploy is admitted with no config diff. Staged rows are pink, green or red, with no "Staged" badge, no "Was" and no Undo; Details discards. Remove copy and a restore that loses writes ask for the typed `project/environment`, titled "Deletes data" and "Data loss". Deploys raise no toast.
- What only adds or prunes points runs at once with a toast: Take snapshot, Keep, Delete snapshot, schedule edits, Refresh now, Upload now and Copy offsite. Delete snapshot and deleting an old copy ask a plain confirm.
- A failed Deploy drops the change that failed. Changes it never reached go back to staged.
- Amended for volume copies: Placement was a form of three rows, Primary, Mirrors and Detached, which named the machinery and hid that a copy is a place to restore from. The Copies view replaces it. Amended earlier for Warm Move: placement once ran at once as Volume Runs from buttons, which put a second apply path beside Deploy.
- Volume Runs (mirror, sync, delete mirror, move, restore) are one Inngest function, `run-volume`, on `attemptLifecycle`. The row is the lock: Postgres lets one active row per Volume in, each row takes a rising `gen` from one sequence, and a run claims its row by id, so a duplicate event exits at the claim. Amended for volume busy: a per-Volume `singleton` once skipped a second run, but it also skipped a taker's run while the yielding one still drained, so the row alone guards the Volume and a `volume/run.taken` event cancels the run whose row was taken. A Deploy starts them from the staged copies, one at a time, and the mirror cron starts syncs. The `volume_run` row is the record users read and Cloud's own bookkeeping, and its `message` is what the bottom bar and the Deployment page show. The run drives each Server with `SetVolumeCopy` requests that name the copy Cloud expects there and the copy it wants, and each Server admits every request against its own copy. At the cut the source stops its Container and becomes a Mirror. One Inngest step then reads the source's full spec and starts the target with `Writer{start}`. Undo works only until that request goes out. From then the commitment outlives its run. It stays owed until a release ends done or a restore takes an epoch, a later release or Deploy finishes it on the target, and moves, mirrors, full syncs and seeds are refused meanwhile. The Copies view asks the Servers what the run's observe step asks them and offers a target only when the run's planner would plan it, never a rule of its own read from run history.

### Servers on older Ployz

A capability name decides and the Release Channel explains. `daemon-compat.ts` is the one file that maps features to capabilities. `requirements` names each feature and the capability it needs, `requirementFor` finds the feature a refusal names, `ServerLacks` is the typed failure a refusal decodes to, and `serverSupports` answers yes, no, or unknown from a Server's record. A record that lists nothing claims no lack.

The Copies view is the one exception. Every daemon that serves `SetVolumeCopy` lists its capabilities, so an empty list proves the lack. A Server whose record lists no capabilities can't move volumes, so Copies asks for an Upgrade. Cloud code holds no release number.

Beside other Servers' lines, the Logs page reads "web-2 needs an upgrade to show its logs." with a link to web-2, or "2 servers need an upgrade to show their logs." with a link to Servers. With no lines at all it reads "Upgrade web-2 to see its logs" with a link to web-2, or "Upgrade 2 servers to see their logs" with a link to Servers. Logs offer no Upgrade and don't watch for one. The reader upgrades from the Server page and reloads Logs.

A Volume Run's lack message comes from the Server's upgrade state, through the same `isBehind`, `isCurrent` and `releaseLine` as `serverUpgradeLine`, so the Servers page and the message agree. Behind the channel release it reads "Volume move needs a newer Ployz on web-2. 0.2.5 is out." Current on the channel it reads "Volume move is not in Ployz 0.2.4 on web-2 yet."

## Voice

- Say the fact in as few words as possible: one short sentence, no semicolons, no clauses that explain themselves.
- No hint unless it changes a decision. A placeholder ("No limit", "Image default") beats a hint line; most rows have none.
- A warning says what's wrong, with the number, then offers the fix as a button. Keep the line short; put the why behind ⓘ.
- Sentence case, verb-first buttons, product words only.

## Do's and Don'ts

### Do:

- **Do** use neutral ink and white for ordinary action hierarchy.
- **Do** carry Intent Pink from changed field through diff and Apply Changes without gaps.
- **Do** keep normal runtime state visually quiet and raise only timely, actionable evidence.
- **Do** distinguish authored product context from runtime truth, especially when testimony is stale or missing.
- **Do** prevent invalid states upstream so Deploy is a confident final action.
- **Do** use compact Geist typography, familiar controls, and complete interaction states.
- **Do** pair every semantic color with text, iconography, shape, or placement that communicates the same meaning.

### Don't:

- **Don't** use purple or violet as a product identity, staged-state, focus, or decorative color.
- **Don't** use Intent Pink for success, warning, failure, informational status, or runtime drift.
- **Don't** resemble verbose, clunky infrastructure administration software.
- **Don't** build self-hosting dashboards that expose their implementation complexity.
- **Don't** create generic pages dominated by forms; disclose only the controls needed for the current decision.
- **Don't** introduce recognizable AI-generated design patterns, including decorative gradients, glass surfaces, oversized rounding, repeated card grids, or ornamental technical imagery.
- **Don't** surface infrastructure detail merely because the detail exists.
- **Don't** put a status line, summary line or banner under a drawer's name. The header is the name.
- **Don't** force users to assemble valid states manually.
- **Don't** turn routine deployment into troubleshooting.
- **Don't** make autosaved edits visually indistinguishable from deployed truth.
- **Don't** put shadows on static cards or pair a one-pixel border with a wide decorative shadow.
- **Don't** rely on color alone for state, focus, validation, or deployment evidence.
