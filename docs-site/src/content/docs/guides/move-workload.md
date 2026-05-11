---
title: Move a Workload
description: Move service placement through migrate commands.
llms:
  summary: Current migrate command forms.
---

Current migration commands operate through the deploy machinery.

Preview a move:

```bash
ployzctl migrate preview service:web --to machine-a
```

Render the generated manifest:

```bash
ployzctl migrate render-manifest service:web --to machine-a
```

Apply the move:

```bash
ployzctl migrate apply service:web --to machine-a
```

The north-star spelling is shorter:

```bash
ployzctl migrate <workload> --to <machine>
```

Use the current subcommand form until the primitive spelling exists.
