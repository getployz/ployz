# Service configuration ownership

Status: Accepted

Service display metadata, deployment policy, and deployable configuration have different lifetimes. Putting them into one authored document made renames and trigger preferences look like runtime changes and let Discard undo credential rotation.

Store display name and deployment policy on the Service identity. Edit them immediately through the metadata command. Keep private DNS independent. Only deployable settings belong to the Environment document and Working/Saved/Applied comparisons. TanStack DB joins these owners for reads; configuration and metadata have separate typed writers.

Saved source/branch and current policy determine automated admission. Recheck under the Environment lock used by policy edits. Waiting CI triggers remain durable and re-evaluate the latest Saved target when resumed. Once admitted, a deployment retains its snapshots.

Configuration references a Service-owned registry credential by stable ID. Rotation replaces encrypted contents and advances credential revision without editing configuration. Admission resolves and freezes those contents; retries preserve the frozen material. No shared credential catalog is introduced.

Core owns deployable configuration and comparison; Cloud owns display metadata, trigger policy, and encrypted credential storage. Existing serialized contracts change together: no compatibility wrapper or ignored-field blacklist.
