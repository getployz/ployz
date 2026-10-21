# Dashboard UI rules

These rules govern the signed-in Ployz Cloud dashboard. The marketing site is built elsewhere and does not follow them.

Every rule here is checkable. A reviewer confirms it from a screenshot or a grep. When the code breaks a rule, fix the code. When a rule is wrong, change it in its own PR and say why.

## Colour

1. **Focus ring is neutral grey.** Focus draws with `--ring`, which is grey. It is never `--changed` or any other state colour.
   To check, tab through a page. No pink outline appears.

2. **Four colours encode state, each with one meaning.**
   - Pink `--changed` means staged. Something was added or changed and is not deployed yet. Pink always comes with a text label.
   - Red `--destructive` means removal or destruction.
   - Blue `--info` means information or work in flight.
   - Green `--success` means healthy.

   No other colour encodes state. A green "New" or a blue "Changed" breaks this rule.
   To check, find every pink element in a screenshot. Each has a word beside it.

## Type

3. **The type scale is 12, 14, 16 and 24px.** Use `text-xs`, `text-sm`, `text-base` and `text-2xl`. No other size, and no arbitrary `text-[...]`.
   To check, grep `src/` for `text-lg`, `text-xl`, `text-3xl` and `text-\[`.

4. **Each page has exactly one h1.** It is 24px semibold (`text-2xl font-semibold`). Section headings are smaller.
   To check, count `<h1>` elements per route. There is one, and no other heading is 24px.

5. **Mono is for machine values only.** Use `font-mono` for IDs, hashes, commands, env keys and values, image refs and logs. Never use it for names, labels, numbers or prose.
   To check, look at names, counts and sizes in a screenshot. They are in the sans face.

## Components

6. **Use the shadcn primitives.** Build from `src/components/ui` and pick their variants and sizes. The shadcn rule in [AGENTS.md](AGENTS.md) bans custom text sizes, padding overrides and ad-hoc colours such as `bg-primary/20`. It is the source for this rule.

7. **One primary action per surface.** A page, panel or dialog has at most one filled button. Every other action is outline, ghost or link.
   To check, count the filled buttons in a screenshot of each surface.

## Destructive actions

8. **Confirm by what is lost, not by the verb.** A staged removal needs no dialog, because Discard undoes it. An action that cannot be undone opens a confirm that names what goes. Deleting real data (an Environment, a project, an organization, a Server) asks the user to type its name.
   To check, press each red button. It either stages a change or opens a confirm.

## Copy

9. **Plain words, short sentences.** Buttons use sentence case and start with a verb ("Generate domain"). Use the terms in [CONTEXT.md](CONTEXT.md), never the machinery behind them (ZFS, Inngest, container IDs in prose). A message is one short sentence. Add a hint line only when it changes what the user decides.
   To check, read the changed strings for Title Case buttons and internal names.

## Where the rest lives

- Tokens live in [src/styles.css](src/styles.css).
- Behaviour lives in the code and its tests.
- User-visible behaviour lives in [docs/user/](../docs/user/).
- Domain terms live in [CONTEXT.md](CONTEXT.md).

Feature PRs do not add component specs to this file. A rule joins it only when it holds across the whole dashboard and a reviewer can check it.

This file replaces the old design system. Three of its rules no longer hold.

- The Product Scale Rule kept type between 12 and 20px. Rule 3 replaces it.
- "The header is the name" forbade anything under a drawer's title. It is gone.
- "Never stack a second title row" kept pages without a heading. Rule 4 gives every page an h1.
