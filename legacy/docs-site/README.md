# Ployz Docs Site

Astro/Starlight documentation site for the open-core Ployz product:
`ployzctl`, `ployzd`, local development, agents, and self-hosted small
clusters.

This site intentionally excludes Ployz Cloud dashboard, billing, team, hosted
machine pool, and managed web UI workflows.

## Run Locally

```bash
cd docs-site
npm install
npm run dev
```

Then visit the URL printed by Astro.

## Build

```bash
cd docs-site
npm run build
```

## Content

Maintain docs as Markdown under `src/content/docs/`.

Generated agent resources:

- `/llms.txt` - short index generated from docs collection metadata
- `/llms-full.txt` - full text bundle generated from the Markdown docs

Do not edit `llms.txt` or `llms-full.txt` by hand. Update Markdown and rebuild.
