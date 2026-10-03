# Branches

A Branch is a copy of a parent Environment that a user changes freely, then saves back to the parent. Behavior: `docs/user/environments/environments.md`.

## Sub-features

- Branch canvas: only the copied services, each marked "New Not deployed"; a link to the parent in the breadcrumb ("Parent: production")
- `link "Branch: N to save"` opens the review at `/<branch>/review`
- Review inspector: link to the parent, button "N changes to save <services>" (expands the diff), **Save to production** (opens the Save sheet)
- **Branch actions** menu: "Keep this branch" (checkbox), "Close fix-api…"
- New branch: `/<env>/new-branch`

## How to get to it (user POV)

From production, use **Create** or the Environment breadcrumb to branch. Open the Branch, then click "Branch: N to save" to review and save.

## Driving it with agent-browser

```bash
agent-browser --session $S open http://localhost:<port>/cloud/ada/shop/fix-api
agent-browser --session $S snapshot -i               # link "Branch: 2 to save" [ref=eN]
agent-browser --session $S click @eN                  # → /cloud/ada/shop/fix-api/review
agent-browser --session $S snapshot -i               # button "Save to production" [ref=eM]
agent-browser --session $S click @eM                  # opens the Save sheet
```

The Save sheet ("2 changes for production") has one region per service, a "Leave out <service> · <setting>" button per change, a switch "Delete fix-api after saving" (on by default), and **Save to production** / **Close**.

## Gotchas

- Seeded state: fix-api copies api and web, with 2 changes to save (api `LOG_LEVEL`, web start command). Production moved on after the branch was made.
- The service order in "2 changes to save api, web" changes between runs; match on "changes to save".
- After **Save to production**, verify on the production canvas through **Details**. The saved values arrive there as unpublished changes.
- Code: `ENV/-components/branch-review/` (StoreBranchPanel, SaveSheet), `ENV/_canvas/review.tsx`, `ENV/_canvas/new-branch.tsx`.
