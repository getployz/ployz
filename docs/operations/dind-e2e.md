# Docker-in-Docker Harness

`testing/ployz-e2e` owns role-neutral Docker provisioning, evidence capture,
and label-scoped cleanup for black-box cluster tests. Run its current compile
and unit gate with:

```sh
scripts/dind-e2e.sh
```

The harness has no product scenario until a v2 slice exposes a public seam
that requires real Docker or multi-process proof. That slice owns adding the
scenario, its fixture assets, and the gated invocation. Deterministic unit or
in-process integration tests remain the default when they can prove the seam.

Every Docker resource created through the harness carries
`dev.ployz.dind.managed=true` and a per-run `dev.ployz.dind.run=<run_id>`
label. Clean abandoned labeled resources with:

```sh
scripts/dind-clean.sh
```

Failed scenarios must capture evidence before teardown. The generic harness
writes evidence beneath `target/dind-evidence/<run_id>/<machine>/`.
