# State Compatibility Lives In Migrations

Ployz runtime code reads only the current persisted state schema. Query,
deploy, gateway, DNS, and operation workers must not carry legacy decoders or
best-effort compatibility branches for old JetStream KV record shapes.

Compatibility belongs in explicit state migrations. A future migration path can
read old schemas, validate preconditions, write the current schema, emit
operation evidence, and fail with typed errors. Until that exists, an alpha
release with an incompatible persisted state change requires an explicit
reset/re-bootstrap or an operator-run one-off migration.

Keeper compatibility checks are a release and development safety signal, not a
normal user recovery path. A blocked check means the target release needs an
explicit migration, reset path, or schema change before it should be pushed to
clusters.

State schema failures should be reported as incompatible state, not as generic
runtime unavailability. This keeps normal runtime paths timeless and makes
compatibility a bounded maintenance surface instead of scattered legacy behavior.
