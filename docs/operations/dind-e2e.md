# Docker-in-Docker Harness

`testing/ployz-e2e` owns role-neutral Docker provisioning, evidence capture,
and label-scoped cleanup for black-box cluster tests. Build the shared machine
image once and run every current public-seam proof with:

```sh
scripts/dind-e2e.sh
```

Independent scenarios run concurrently after one Cargo compile. The worker
count defaults to the smaller of four and the local CPU count; set
`PLOYZ_DIND_JOBS=1` when debugging interleaved output. Machine containers and
joiners inside one scenario are also provisioned concurrently. Each run keeps
its own Docker network, names, labels, and evidence directory.

The machine image preloads only the nginx and registry fixtures used by the
current scenarios. Add another pinned workload only when a public-seam proof
actually consumes it; every baked image is imported by every DinD machine and
therefore sits directly on the startup path.

Deterministic unit or in-process integration tests remain the default when
they can prove the seam.

Every Docker resource created through the harness carries
`dev.ployz.dind.managed=true` and a per-run `dev.ployz.dind.run=<run_id>`
label. Clean abandoned labeled resources with:

```sh
scripts/dind-clean.sh
```

Failed scenarios must capture evidence before teardown. The generic harness
writes evidence beneath `target/dind-evidence/<run_id>/<machine>/`.
