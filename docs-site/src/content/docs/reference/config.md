---
title: Config and Paths
description: Config, data directory, and socket flags.
llms:
  summary: Global config flags.
---

Current forwarded daemon commands accept global flags:

```bash
ployzctl --config <PATH> --data-dir <PATH> --socket <PATH> --json status
```

Use `--config` to select a daemon config file, `--data-dir` to select runtime
state location, and `--socket` to point at a daemon socket.

For automation, combine these with `--json` so the caller receives structured
daemon responses.
