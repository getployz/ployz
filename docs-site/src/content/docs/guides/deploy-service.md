---
title: Deploy a Service
description: Deploy with current manifest and service command forms.
llms:
  summary: Current deploy entrypoints.
---

Current deploy flows use either a manifest file or a specific subcommand.

Preview a manifest:

```bash
ployzctl deploy --file ployz.toml --dry-run
```

Apply a manifest:

```bash
ployzctl deploy --file ployz.toml
```

Deploy a service from arguments:

```bash
ployzctl deploy service web --image ghcr.io/acme/web:latest --namespace prod
```

After a deploy, verify status:

```bash
ployzctl --json status
```
