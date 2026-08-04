# Machine Updates

The coreless v2 workspace does not yet expose a machine-update operation. The
incumbent control-plane and Host Runner update flows are outside the current
architecture.

When machine updates return, their design must follow Keeper's charter and the
explicit-operation rules: one exact target version, bounded work, durable
progress, one terminal result, and useful local evidence on failure. Keeper may
converge machine substrate only toward an operator decision already recorded in
Corrosion; it may not choose versions or mutate cluster truth in the background.

The update slice also owns its public HTTP contract, artifact provenance,
atomic activation strategy, rollback evidence, and real-host acceptance path.
