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
- Core-owned evidence and Cloud-owned product context presented without blurring authority.
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

**The Neutral Action Rule.** Ordinary primary actions are ink on light surfaces and white on dark surfaces. Chromatic fills never become a generic importance shortcut.

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
- The Environment switcher shows that tree, notes "default" and "not deployed", and on a Branch "N to save" (what Save would put in its Parent) and "N updates" (what's new there). On a Branch it offers **Review X**, which opens the Branch's review page. It ends with **New branch of X**, which opens the New branch panel over the current Environment's canvas, and **Manage environments**, which opens Settings → Project.
- Settings has two tabs, **Environment** and **Project**, each linkable. Project holds the Default Environment picker, the project's Environment tree (each with its services-online summary and a Default chip, opening that Environment) with **New branch** (of the current Environment) and **New environment** (an empty root Environment) above it, and project teardown. Environment holds this Environment's teardown.
- The canvas's **Find** button and the `/` key open the resource finder; `/` never fires while typing in a field.
- Icon-only controls always have an accessible name and a tooltip.

**The Soon rule.** An option that isn't built yet appears only inside a flow that works, greyed out with a Soon tag. It is never a page, tab or button that does nothing.

### Branches

- A Live Node is drawn dashed and translucent, labelled with the Environment it comes from ("production's, live"), and links into it are dashed; every other link is solid. Opening it says whose it is, which services here use it, and opens it in its own Environment.
- A Live Node that owns data carries an amber "real data" line.
- While a Branch is being picked, the canvas is the picker: clicking a card toggles its Own Copy. Own Copies are lit, Live Nodes dashed and left-out nodes faded, and the panel shows the legend. Phones pick with a tick list.

### Apply Changes

The staged-change system connects edited fields, affected resources, and environment-wide review. Every screen size places the change count, Review and Deploy in the bottom bar. Review replaces the workspace rather than stacking a dialog over a resource inspector. Save publishes configuration without deployment; Deploy publishes and starts deployment. These are distinct visible review actions. Returning from review restores the editor and canvas context.

### Deployments

Deployments are a place, not a mode of the canvas. Each Cloud Deployment Attempt has its own Deployment Page, which opens as a panel over the canvas; the canvas stays mounted underneath and always draws the Environment as it is now. Closing the page leaves the canvas, its selection and its viewport as they were.

- While the page is open, the canvas lights up what the attempt changed: those nodes show their Node Outcome and the rest dim. Nodes it removed, or that were deleted since, appear only in the page's list.
- The page's header places the attempt: its message, what triggered it and who, the Git branch and commit, the status, the duration and the age. Its actions follow the status.
- One chip per changed service picks whose logs show; past six they become a dropdown. **Build | Deploy** tabs follow the running stage: Build while building, Deploy once deploying, the failed stage on failure, until the user picks one. Build is disabled for a prebuilt image.
- A manual Deploy opens its page when the user's "open started deployments" setting is on. Git-triggered deployments never take over the screen, and nothing returns the user to the canvas automatically. Deploying while another deployment runs queues.

One floating **bottom bar** sits at the bottom of the canvas on every screen size and stays usable while a panel is open. Its first row is the Environment itself, the first that applies:

1. **A starting point:** "X isn't deployed" and **New branch**. Its staged nodes are what Branches of it copy, not pending work, so the bar doesn't count them.
2. **Changes to deploy.** The row takes the staged-intent surface: "N changes to deploy", the one change or the changed services' names, **Details · Deploy ⇧+Enter · ⋮** (⋮ holds Discard). Deploy reads **Deploy next** while another attempt runs or waits, because deploying then queues. Git-triggered deployments never clear these changes.
3. **A running or queued attempt** whose Deployment Page isn't open: its status and message, the service and step it is on, and **Logs**, which opens its page.
4. **A shutdown:** "Shutting down" while it runs; then "Off · Starts again on the next push" and **Deploy**, its services reading Off and the Environments list saying Off; or "Shutdown failed" and **Shut down** again.
5. **Changes that go live with a pull request:** the quiet "3 changes go live with PR #142", the services they touch, and **Details**. They aren't changes to deploy.

On a Branch a second row holds what it has for its Parent X, whatever the first row shows, so Save is always one tap away: "N changes to save · into X" and **Save**, else "N updates from X" and **Update** (**Details** while Update must wait). A PR Environment has one such row per Destination: "3 changes to save · go live when PR #142 merges" and **Save**, then "3 changes go live · when PR #142 merges" and **Details**, which holds Undo. A row with nothing is absent, and a root has one row. The rows sit side by side on desktop and stack on phones.

A Branch's review page is a panel over its canvas with the rest of its relationship to its Parent: **New in X** (including Live Nodes their owner redeployed since this Branch last deployed) and **Stays different**, each with a plain reason. A conflict, a setting the Parent also changed since branching, reads old → new with a marker.

**Save** never deploys and never waits for a deploy: it takes what the Branch has, deployed or not, while it deploys, after a failed deploy, or from a starting point. Its sheet is titled by the count ("3 changes for production"). Each service says what happens to it ("api will be updated", "cache will be added") and lists its settings as Change · Current · New: × leaves a change out, and tapping New gives X its own value; a new secret asks for X's value. One line says what happens next ("Nothing deploys yet. production gets 3 changes to deploy."), and one button says **Save to X**. The changes become X's changes to deploy, and the user lands on X's canvas. **Delete fix-api after saving** is on by default; a Kept Branch and the Default Environment don't show it. Deleting a Branch that is deploying cancels the deploy first. Update keeps its wait: it rewrites what the Branch runs.

On a PR Environment the same sheet adds "PR #142 · {title} ↗" under the title, and its line says "web and worker redeploy when PR #142 merges"; the button is still **Save to X**. **Shut down pr-142 now · Starts again on the next push** is a switch, off by default; the Environment switcher's **Shut down pr-142** does the same at any time. Saved changes wait on X, not staged there: X's Details list them read-only and neutral, never in the staged-intent color. When the pull request merges they are ordinary saved changes, except a setting X changed too: X's value stays, and the pull request's value is a change to deploy tagged "PR #142", or, beside an edit X hasn't deployed, a one-line hint "PR #142: {value} · Use". There is no pick dialog.

Bar text stays minimal: fewer words on mobile, and explanations belong in a panel, never in the bar.

The canvas lays itself out; nodes are never dragged. When an edit moves a node to a new place, it glides there in about 200ms, instantly under reduced motion. That glide is functional, not flair: it has no tint or highlight and only answers "where did it go?".

**The One Vocabulary Rule.** A state looks and behaves the same in every field, resource, drawer, diff row, and toolbar. Local reinvention is a defect.

## Do's and Don'ts

### Do:

- **Do** use neutral ink and white for ordinary action hierarchy.
- **Do** carry Intent Pink from changed field through resource, diff, and Apply Changes without gaps.
- **Do** keep normal runtime state visually quiet and raise only timely, actionable evidence.
- **Do** distinguish Cloud-owned product context from core-owned runtime truth, especially when testimony is stale or missing.
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
