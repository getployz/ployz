---
title: Current Commands
description: Current executable command groups for ployzctl and forwarded ployzd commands.
llms:
  summary: Current command surface and exact executable forms.
---

`ployzctl daemon install` is implemented directly by `ployzctl`. Other current
operator commands are forwarded to `ployzd`.

## Native `ployzctl`

```bash
ployzctl daemon install --runtime docker --service-mode user
ployzctl daemon install --runtime host --service-mode system
```

## Forwarded `ployzd` groups

```bash
ployzctl run
ployzctl --json status
ployzctl doctor
```

```bash
ployzctl deploy --file <path>
ployzctl deploy --file <path> --dry-run
ployzctl deploy preview --file <path>
ployzctl deploy service <name> --image <image> --namespace <namespace>
```

```bash
ployzctl migrate apply <service_ref> --to <machine>
ployzctl migrate preview <service_ref> --to <machine>
ployzctl migrate render-manifest <service_ref> --to <machine>
```

```bash
ployzctl machine ls
ployzctl machine add <target>...
ployzctl machine rm <id>
ployzctl machine drain <target>
ployzctl machine standby <target>
ployzctl machine rtt
ployzctl machine invite create|list|revoke|import
```

```bash
ployzctl image status|push|distribute|inspect|operation
```

Use `ployzd --help` or `ployzd <group> --help` for the exact flags supported by
the current binary.
