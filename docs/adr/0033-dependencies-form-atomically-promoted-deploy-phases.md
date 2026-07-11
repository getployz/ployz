# Dependencies Form Atomically Promoted Deploy Phases

Service dependencies are typed `started` or `healthy` edges that derive
topological deploy phases. Every service eligible at the same point belongs to
the same phase. `started` does not bypass the dependency service's own creation
gate, and `healthy` is invalid unless the dependency defines an executable
healthcheck. Run-to-completion dependencies remain outside the deploy model.

Execution promotes one successful phase at a time. One intent transaction
commits all Serving Target entries and route-binding changes for that phase,
then emits one invalidation. Later phases use ordinary internal DNS backed by
that promoted intent; deploys do not create provisional or operation-scoped DNS
truth.

A failed phase commits none of its intent changes. Containers or hooks that
actually failed remain as evidence, successfully started but unpromoted
containers from that phase are removed, and reused containers and earlier
promoted phases remain untouched. Earlier promotion makes the deploy outcome
partial; failure before any promotion makes it failed.
