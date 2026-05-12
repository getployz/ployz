---
status: completed
module: docs
created: 2026-05-11
tags:
  - docs
  - cli
  - website
problem_type: feature
---

# Open Core Docs Site

## Problem

Ployz needs a public documentation site for the open-core CLI, daemon, and
self-hosted runtime. The separate cloud/web UI product will have its own docs,
so this site must stay focused on `ployzctl`, `ployzd`, local development, and
small self-hosted clusters.

The first version should establish a strong information architecture and a real
usable web experience without adding unnecessary framework weight to the Rust
repo.

## Scope

- Add an Astro/Starlight docs site under `docs-site/`.
- Present restrained navigation for the open-core product:
  - Get started
  - Concepts
  - Commands
  - Guides
  - Security
  - Agents
  - Reference
- Include starter content grounded in `VISION.md`, `docs/architecture.md`, and
  the current `ployzctl` surface.
- Generate `llms.txt` and `llms-full.txt` from Markdown docs for
  agent-readable documentation.
- Document how to serve the site locally.

Out of scope:

- Cloud dashboards, billing, hosted machine pools, team workflows, and managed
  web UI docs.
- A full generated CLI reference pipeline from Clap.
- Versioned docs infrastructure.
- Converting existing internal planning docs into polished public docs.

## Key Decisions

1. Use Astro/Starlight for v1.
   - Rationale: the user requested Markdown files that automatically generate a
     site from a good docs template. Starlight provides navigation, page
     outlines, dark mode, and a maintainable Markdown-first structure.

2. Put public docs under `docs-site/`, not `docs/`.
   - Rationale: `docs/` already contains internal architecture, plans, and
     solutions. Keeping the public site separate avoids mixing polished product
     docs with working notes.

3. Make agent documentation first-class.
   - Rationale: `VISION.md` explicitly frames concrete foreground commands,
     structured output, typed failures, and verification hooks as useful to both
     humans and agents.

4. Keep the cloud product out of this IA.
   - Rationale: the user clarified that web UI cloud docs are separate. This
     site may mention that separation once, but should not explain cloud flows.

## Implementation Units

### Unit 1: Astro/Starlight Site Shell

Files:

- `docs-site/package.json`
- `docs-site/astro.config.mjs`
- `docs-site/src/content.config.ts`
- `docs-site/src/styles/custom.css`
- `docs-site/README.md`

Behavior:

- Use Starlight for navigation, page outlines, dark mode, and standard
  docs layout.
- Keep custom styling light and product-specific.
- Disable Pagefind search for now because the local Pagefind binary crashes in
  this environment after route generation. Re-enable search when the deployment
  target is known to support it or a replacement search strategy is selected.

Test scenarios:

- `npm run build` succeeds.
- Preview the built site and verify desktop/mobile layout.

### Unit 2: Markdown Starter Content

Files:

- `docs-site/src/content/docs/**/*.md`

Behavior:

- Add concise open-core docs content split into logical Markdown files:
  - product boundary and non-cloud note,
  - quickstart commands,
  - operator-loop concepts,
  - current and north-star command surface,
  - focused guides list,
  - security/trust boundary notes,
  - agent usage model,
  - reference links back to repo docs.

Test scenarios:

- Verify no cloud-specific workflows are documented.
- Verify all command examples are clearly marked as current, planned, or
  north-star where needed.
- Verify copy stays concise enough to scan and individual pages are easy to
  maintain.

### Unit 3: Generated Agent-Readable Docs Bundle

Files:

- `docs-site/src/lib/docs.ts`
- `docs-site/src/pages/llms.txt.ts`
- `docs-site/src/pages/llms-full.txt.ts`

Behavior:

- Generate a short agent index and a fuller text bundle from Markdown docs and
  collection metadata.

Test scenarios:

- Build and preview both generated endpoints as plain text.
- Verify they do not include cloud UI instructions.

## Verification

- `npm install` in `docs-site/`.
- `npm run build` in `docs-site/`.
- Open the built site locally with Astro preview.
- Browser screenshot checks for desktop and mobile.
- If practical, run `just test` only if changes touch Rust or repo behavior;
  otherwise document that the site is static and Rust tests were not run.

## Risks

- The current CLI implementation is much smaller than the north-star command
  surface. Mitigation: label current vs. planned primitives honestly.
- Introducing a docs toolchain adds npm dependencies. Mitigation: keep it
  scoped under `docs-site/` so the root installer package remains unchanged.
