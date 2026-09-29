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

This design system governs the authenticated product dashboard. The public `/` lander is a separate brand surface and must not drive product density, component behavior, or visual hierarchy. Product UI is restrained rather than theatrical: familiar controls, compact information, progressive disclosure, and state transitions that explain what just changed.

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

- **Intent Pink** (`#d0268c`): the saturated anchor for staged intent, selection emphasis, and focus. It is deliberately warm and unmistakably pink, never violet or purple.
- **Intent Deep** (`#a80068`): accessible intent text and compact indicators on pale staged surfaces.
- **Intent Soft** (`#fff0f7`) and **Intent Border** (`#efb3ce`): the background and boundary applied to autosaved fields, resources, and diff rows that differ from deployed truth.

### Tertiary

- **Evidence Green** (`#42946e`): successful or healthy runtime evidence.
- **Attention Amber** (`#ad871f`): warnings and consequences that require consideration.
- **Failure Red** (`#b62d2b`): errors, invalid state, and destructive intent.
- **Information Blue** (`#2057c5`): neutral informational state and links where surrounding context does not already establish interactivity.
- Each semantic hue has a pale companion surface. Text, iconography, and state language must accompany the color.

### Neutral

- **Clear Canvas** (`#ffffff`): the default page and component surface.
- **Quiet Surface** (`#fafafa`) and **Muted Surface** (`#f2f2f2`): secondary structure, selected navigation, toolbars, and disabled regions.
- **Muted Ink** (`#666666`): supporting copy that still meets contrast requirements.
- **Structural Rule** (`#dedede`): borders and dividers that clarify grouping without becoming decoration.
- Dark mode uses neutral black surfaces (`#0a0a0a`, `#161616`) and neutral light ink (`#ededed`); it must not reintroduce a violet cast.

**The Neutral Action Rule.** Ordinary primary actions are ink on light surfaces and white on dark surfaces. Chromatic fills never become a generic importance shortcut. The one exception is the bottom bar's **Deploy**, solid Intent Pink: it is where a staged change's pink trail ends.

**The Visible Intent Rule.** Pink follows an autosaved change from its field to its resource, diff, and apply surface. Saturated pink is rare; most staged state uses the soft surface, border, and deep text.

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

### Projects

- Label the organization destination and page **Projects**. Keep search visible and use an ink **New project** action.
- Use three columns on desktop, two on tablet, and one on mobile.
- Each project card shows the Default Environment's Working State Services as centered icons on a subtle dotted surface. Reuse existing source icons; do not use canvas positions, connections, or Volumes.
- The whole card opens that Environment. Keep card controls out of the preview.
- Footer: `● production · 2/3 services online`; an empty Environment shows `production · No services`. Count a Service once when it has a running container whose health is healthy or not configured. Exclude hooks. When runtime evidence is disconnected or incomplete, show only the service count.

### Inputs / Fields

- **Style:** 32px controls, 12px corners, transparent or canvas background, one-pixel Structural Rule border, and 10px horizontal padding.
- **Focus:** visible three-pixel Intent Pink ring plus a stronger boundary; focus never depends on subtle color shift alone.
- **Staged:** an autosaved value that differs from deployed truth uses Intent Soft, Intent Border, and a restrained pink ring. The treatment persists after blur.
- **Error / Disabled:** invalid state replaces staged emphasis with Failure Red; disabled fields use a muted surface and reduced emphasis without becoming unreadable.

### Navigation

- An Environment has four places: **Architecture** (its canvas), **Deployments**, **Logs** and **Settings**. The organization has three: **Projects**, **Servers** and **Organization**, its own settings. No two places share a name, so Settings always means an Environment's.
- On desktop a rail holds the logo, the scope's places, each an icon beside its label, and the avatar at the bottom. On an Environment, Projects and Servers follow its places below a divider. Wherever the canvas shows, the rail narrows to its icons, each naming itself on hover, and the canvas gets the room. The current place uses a muted neutral surface, never the staged-intent color.
- A page with sections lists them under its place in the rail while it's current: Settings has **Environment** and **Project**; Organization has **General**, **Builds**, and **Billing** where billing exists. A page never repeats its sections as tabs.
- On phones the scope's places fill a bottom tab bar, and the current place's sections sit in a strip under the top bar.
- The logo tops the rail and opens Projects. The avatar holds organization switching, Theme and Log out.
- One top bar per page. On an Environment it says where you are with breadcrumbs, `project / environment`, plus the place's name when it isn't Architecture. On a Branch the path reads `project / parent ⑂ branch`: ⑂ marks what the Branch was made from, and the Parent's crumb opens the Parent. Each switcher crumb opens its switcher, and switching keeps the current place. On phones the bar also carries the logo and the avatar, the path keeps its last two crumbs and moves the rest into a "…" menu, and the bar never wraps. Never stack a second title row that repeats the place.
- Wherever Environments are listed, they form one tree: root Environments first, each Branch indented under its Parent and marked ⑂.
- The Environment switcher shows that tree with each Environment's notes: "default", "not deployed", "kept", a PR Environment's "PR #N" and "Off", and on a Branch "N to save" (what Save would put in its Parent) and "N updates" (what's new there). On a Branch it offers **Manage X**, which opens the Branch's panel. It ends with **New branch of X**, which opens the New branch panel over the current Environment's canvas, and **Manage environments**, which opens Settings → Project.
- Settings has two sections, **Environment** and **Project**, each linkable. Project is where every Branch is seen and how Branches work is set up: the Default Environment picker, the project's Environment tree (each with its services-online summary, the switcher's notes and a Default chip, opening that Environment) with **New branch** (of the current Environment) and **New environment** (an empty root Environment) above it, PR environments, and **Delete project**. Environment holds what's about the Environment itself: its branch defaults and **Delete environment**. On a Branch it holds no Branch controls, only a row that opens its panel.
- The canvas's **Find** button and the `/` key open the resource finder; `/` never fires while typing in a field.
- Icon-only controls always have an accessible name and a tooltip.

**The Soon rule.** An option that isn't built yet appears only inside a flow that works, greyed out with a Soon tag. It is never a page, tab or button that does nothing.

### Branches

- **The top moves changes between Environments; the bottom bar stays in this one.** Save and Update bring changes from one Environment into another, where they land as ordinary changes to deploy: Update in the Branch's own bottom bar, Save in its Parent's, where the user lands. So neither sits in a bottom bar.
- **Two places, each with one job.** Acting on this Branch happens in its **panel**, over its canvas: where it came from, what Save would put in its Parent, what's new there, and its check on GitHub, with Keep, Shut down and Close in the panel's ⋮. Seeing every Branch and setting up how Branches work happens in Settings → Project. Nothing else holds a Branch control.
- **Inside a Branch, few fork words.** A Branch is an Environment with a way to move changes to and from its Parent. Its panel does only that; what it keeps apart from its Parent, and how it ends, stay out of the way.
- On a Branch the canvas's top right has the **Branch button** before Find: ⑂ and the first that applies, "Shutdown failed", "3 to save", "1 update", "Shutting down" or "Off", "Closes in 2 days", "Saved", else "Up to date". It opens the panel, so the panel is one tap from anywhere on the canvas, phones included.
- A Live Node is drawn dashed and translucent, labelled with the Environment it comes from ("production's"), and links into it are dashed; every other link is solid. Opening it says whose it is, which services here use it, and opens it in its own Environment.
- A Live Node that owns data carries an amber "real data" line.
- While a Branch is being picked, the panel lists each of its Parent's nodes beside what the Branch gets; tapping one offers its two choices. On desktop the canvas picks too: clicking a card toggles it, separate ones are lit, Live Nodes dashed and left-out nodes faded.

### Destructive actions

Guard what can't come back, never the verb.

- **It comes back on its own**, like a PR Environment whose pull request is open: no dialog. Its next push brings it back.
- **It's staged**, like deleting a service, volume, variable, domain or mount: no dialog. The Review lists it and Discard undoes it.
- **A Branch that isn't kept**: one plain confirm that names its Own Copies, which started empty.
- **The root of real data**: an Environment, a Kept Branch, a project, an organization, a server, or a save or deploy that deletes outside a Branch that isn't kept. One dialog lists what goes by name with its canvas icon, and the user types where it is: `project/environment`, the project, the organization or the server. Up to four things are all named. Past that, anything new and the two biggest volumes are named, and the rest count per kind, a click from their names. Where Cloud knows what goes, the list opens at once with it, and anything the servers add is marked new; a list only the servers know, like a server's volumes, has nothing to be new against. The servers add sizes where they report them, and the button waits for them.
- **Locks are structural.** The Default Environment can't be deleted; nothing has a protection switch.
- **Words:** Branches and PR Environments close, servers are removed, and everything else is deleted. Confirmations name things in the user's words and never Ployz's machinery.
- A failure only Ployz can fix says what failed and offers Retry; it never leaves a dialog that can't go forward.

### Apply Changes

The staged-change system connects edited fields, affected resources, and environment-wide review. Every screen size places the change count, Review and Deploy in the bottom bar. Review replaces the workspace rather than stacking a dialog over a resource inspector. Publish puts configuration in Saved State without deploying; Deploy publishes and starts deployment. These are distinct visible review actions. Returning from review restores the editor and canvas context.

### Deployments

Deployments are a place, not a mode of the canvas. Each Deployment has its own Deployment Page, which opens as a panel over the canvas; the canvas stays mounted underneath and always draws the Environment as it is now. Closing the page leaves the canvas, its selection and its viewport as they were.

- While the page is open, the canvas lights up what the attempt changed: those nodes show their Node Outcome and the rest dim. Nodes it removed, or that were deleted since, appear only in the page's list.
- The page's header places the attempt: its message, what triggered it and who, the Git branch and commit, the status, the duration and the age. Its actions follow the status.
- One chip per changed service picks whose logs show; past six they become a dropdown. **Build | Deploy** tabs follow the running stage: Build while building, Deploy once deploying, the failed stage on failure, until the user picks one. Build is disabled for a prebuilt image.
- A manual Deploy opens its page when the user's "open started deployments" setting is on. Git-triggered deployments never take over the screen, and nothing returns the user to the canvas automatically. Deploying while another deployment runs queues.

One floating **bottom bar** sits at the bottom of the canvas on every screen size and stays usable while a panel is open. It holds this Environment's own changes and nothing else, in one row, the first that applies: its words, then its actions, at full control size, like Railway's. It sizes to its content up to 36rem; its lines truncate, and values live in Details.

1. **Changes to deploy.** The row takes the staged-intent surface: "Apply N changes", then **Details · Deploy · ⋮** (⋮ holds Discard). Deploy is solid pink, and its tooltip says ⇧+Enter. What changed shows on the canvas and in Details, never in the bar. Deploy reads **Deploy next** while another attempt runs or waits, because deploying then queues. Git-triggered deployments never clear these changes. An Environment that has never deployed shows all its nodes here.
2. **A running or queued attempt** whose Deployment Page isn't open: its status and message, the service and step it is on, and **Logs**, which opens its page.
3. **Changes that go live here with a pull request:** the quiet "3 changes go live with PR #142", the services they touch, and **Details**. They aren't changes to deploy.

Nothing about a Branch sits in the bar: Save, Update, a shutdown and closing are the Branch button's.

A Branch's panel answers one question first: what to do next. It leads with the Branch's first news, as the Branch button says it, in a card holding the panel's only solid button. The rest of its news follows a line each, most pressing first:

- **N changes to save**, naming the services (on a PR Environment one per Destination, "go live when #142 merges"), with **Save to X**.
- **N updates from X**, naming the services, including Live Nodes their owner redeployed since the Branch last deployed, with **Update**; while it must wait, the line says why instead. "N changed in fix-web too", in amber, warns of settings both sides changed.
- A shutdown: **Shut down** again after a failure, "Shutting down", or "Off" with **Deploy pr-142**.
- "Closes in N days" with **Keep it**.
- **Saved for X**, going live when the pull request merges, with **Undo**.
- Else "Up to date with X".

A line with changes opens to them, each old → new, a conflict marked; values never crowd the lines. The pull request's check on GitHub follows. Keep, Shut down and Close wait in the panel's ⋮: a Branch that isn't kept closes after one plain confirm, a PR Environment with an open pull request closes at once until its next push, and the rest confirm typed; a typed close's progress shows at the top of the panel.

**Save** never deploys and never waits for a deploy: it takes what the Branch has, deployed or not, while it deploys, after a failed deploy, or before it ever deployed. Its sheet is titled by the count ("3 changes for production"). Each service says what happens to it ("api will be updated", "cache will be added") and lists its settings as Change · Current · New: × leaves a change out, and tapping New gives X its own value; a new secret asks for X's value. One line says what happens next ("Nothing deploys yet. production gets 3 changes to deploy."), and one button says **Save to X**. The changes become X's changes to deploy, and the user lands on X's canvas. **Delete fix-api after saving** is on by default; a Kept Branch and the Default Environment don't show it. Deleting a Branch that is deploying cancels the deploy first. Update keeps its wait: it rewrites what the Branch runs.

On a PR Environment the same sheet adds "PR #142 · {title} ↗" under the title, and its line says "web and worker redeploy when PR #142 merges"; the button is still **Save to X**. **Shut down pr-142 now · Starts again on the next push** is a switch, off by default; the panel's **Shut down pr-142** does the same at any time. While it shuts down the panel says so, then "Off", with **Deploy pr-142**, its services reading Off and the Environments list saying Off; a failed shutdown offers **Shut down** again. Saved changes wait on X, not staged there: X's Details list them read-only and neutral, never in the staged-intent color. When the pull request merges they are ordinary saved changes, except a setting X changed too: X's value stays, and the pull request's value is a change to deploy tagged "PR #142", or, beside an edit X hasn't deployed, a one-line hint "PR #142: {value} · Use". There is no pick dialog.

Bar text stays minimal: fewer words on mobile, and explanations belong in a panel, never in the bar.

**The One Vocabulary Rule.** A state looks and behaves the same in every field, resource, drawer, diff row, and toolbar. Local reinvention is a defect.

## Do's and Don'ts

### Do:

- **Do** use neutral ink and white for ordinary action hierarchy.
- **Do** carry Intent Pink from changed field through resource, diff, and Apply Changes without gaps.
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
- **Don't** force users to assemble valid states manually.
- **Don't** turn routine deployment into troubleshooting.
- **Don't** make autosaved edits visually indistinguishable from deployed truth.
- **Don't** put shadows on static cards or pair a one-pixel border with a wide decorative shadow.
- **Don't** rely on color alone for state, focus, validation, or deployment evidence.
