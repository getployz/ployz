# ployz

Ployz is a Rust orchestration core for explicit infrastructure operations on
small clusters. The active rewrite lives at the repository root and starts with
two crates:

- `crates/ployz`: product orchestration code for deploys, domains,
  certificates, serving, runtime, and volumes.
- `crates/polis`: the internal support framework for product-neutral
  authority, records, projections, operation evidence, claims, and bounded
  calls.

`legacy/` contains previous implementations used as reference material. It is
not part of the active root workspace.

## Boundary

Ployz owns product meaning. Polis owns only the reusable distributed
control-plane primitives needed to keep Ployz code small and readable. Ordinary
Ployz feature modules must not import Polis directly; direct Polis usage belongs
in adapters and composition code.

The current proof covers:

- `ployz domain add <hostname>` semantics for HTTPS readiness, with structured
  certificate and serving readiness status;
- deploy with HTTPS domain readiness, runtime activation, serving commit, and
  activation verification;
- ACME challenge ownership using product mutation context;
- volume transfer with source fencing, ownership verification, and visible
  cleanup-pending status; and
- boundary checks that keep legacy, raw substrate, and product imports out of
  the wrong layers.

## Development

```bash
just check
```

`just check` runs formatting, all workspace tests, and the dependency boundary
guard.
