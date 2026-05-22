use crate::error::{MachineFailure, PrimitiveFailure};
use crate::facts::{
    ProductFact, ProductFactAppendOutcome, ProductFactConflict, ProductFactRejection,
    machine::{
        MACHINE_JOINED_KIND, MACHINE_REMOVAL_STARTED_KIND, MACHINE_TOMBSTONED_KIND,
        MachineFactError, MachineMembershipFact, MachineMembershipReducer,
    },
};
use crate::machine::{MachineId, MachineMembership, MachineMembershipPort, MachineStatus};
use crate::operation::MutationContext;

#[derive(Debug)]
pub(crate) struct PolisMachineMembership<S> {
    projection: polis::MemoryProjectionSource<S>,
}

impl<S> PolisMachineMembership<S> {
    #[must_use]
    pub(crate) fn new(projection: polis::MemoryProjectionSource<S>) -> Self {
        Self { projection }
    }
}

impl PolisMachineMembership<polis::MemoryFactStore> {
    #[must_use]
    pub(crate) fn in_memory() -> Self {
        Self::new(polis::MemoryProjectionSource::new(
            polis::MemoryFactStore::new(),
        ))
    }
}

impl<S> MachineMembershipPort for PolisMachineMembership<S>
where
    S: polis::FactStore,
{
    fn observe(
        &self,
        context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        self.project_machine_status(context, machine)
    }

    fn join(
        &self,
        context: &MutationContext,
        membership: &MachineMembership,
    ) -> Result<MachineMembership, MachineFailure> {
        let fact = MachineMembershipFact::joined(membership.clone());
        match self.append_machine_fact(context, &fact)? {
            ProductFactAppendOutcome::Appended(_) | ProductFactAppendOutcome::Replayed(_) => {}
            ProductFactAppendOutcome::Conflict(conflict) => {
                return Err(machine_conflict_failure(conflict, membership));
            }
            ProductFactAppendOutcome::Rejected(ProductFactRejection::Unauthorized) => {
                return Err(MachineFailure::MutationRejected);
            }
        }

        match self.project_machine_status(context, &membership.machine)? {
            MachineStatus::Joined(joined) if joined == *membership => Ok(joined),
            MachineStatus::Joined(joined) => Err(MachineFailure::MembershipConflict {
                machine: joined.machine,
                epoch: Some(joined.epoch),
            }),
            MachineStatus::Conflicted { machine, epoch } => {
                Err(MachineFailure::MembershipConflict {
                    machine,
                    epoch: Some(epoch),
                })
            }
            MachineStatus::Removing(removal) => Err(MachineFailure::MachineRemoving {
                machine: removal.machine,
                epoch: removal.epoch,
            }),
            MachineStatus::Tombstoned(tombstone) => Err(MachineFailure::MachineTombstoned {
                machine: tombstone.machine,
                epoch: tombstone.epoch,
            }),
            MachineStatus::Absent => Err(MachineFailure::MutationMismatch {
                machine: membership.machine.clone(),
            }),
        }
    }
}

impl<S> PolisMachineMembership<S>
where
    S: polis::FactStore,
{
    fn append_machine_fact(
        &self,
        context: &MutationContext,
        fact: &MachineMembershipFact,
    ) -> Result<ProductFactAppendOutcome, MachineFailure> {
        let envelope = fact.encode().map_err(map_machine_fact_error)?;
        let target = super::polis_fact_target(envelope.target()).map_err(map_machine_primitive)?;
        let payload =
            super::polis_fact_payload(envelope.payload()).map_err(map_machine_primitive)?;
        let authority =
            super::context_fact_append_authority(context).map_err(map_machine_primitive)?;
        let grant = polis::FactGrantService::new(MachineFactGrantAuthority)
            .issue_append(&authority, target.clone())
            .map_err(map_machine_polis_error)?;
        let request = polis::FactAppendRequest::new(
            polis::OperationId::parse(format!(
                "{}:{}",
                context.operation().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_machine_polis_error)?,
            polis::IdempotencyKey::parse(format!(
                "{}:{}",
                context.idempotency().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_machine_polis_error)?,
            authority,
            grant,
            target,
            payload,
            None,
        );
        let outcome = polis::FactStore::append(self.projection.facts(), request)
            .map_err(map_machine_polis_error)?;
        super::product_fact_append_outcome(outcome).map_err(map_machine_primitive)
    }

    fn project_machine_status(
        &self,
        context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        let request = polis::ProjectionRequest::new(
            machine_projection_view(machine)?,
            machine_fact_query(context, machine)?,
            PolisMachineReducer {
                reducer: MachineMembershipReducer::new(machine.clone()),
            },
        );
        polis::ProjectionSource::project(&self.projection, request)
            .map_err(map_machine_projection_error)
            .and_then(|snapshot| {
                super::require_fresh_projection(snapshot, |_| MachineFailure::ProjectionUnavailable)
            })
    }
}

fn machine_conflict_failure(
    conflict: ProductFactConflict,
    desired: &MachineMembership,
) -> MachineFailure {
    match conflict {
        ProductFactConflict::KeyPayloadConflict { .. }
        | ProductFactConflict::RejectedKeyPayloadConflict { .. } => {
            MachineFailure::MembershipConflict {
                machine: desired.machine.clone(),
                epoch: Some(desired.epoch),
            }
        }
        ProductFactConflict::IdempotencyKeyReuse { .. } => MachineFailure::MutationRejected,
    }
}

struct PolisMachineReducer {
    reducer: MachineMembershipReducer,
}

impl polis::FactReducer for PolisMachineReducer {
    type View = MachineStatus;
    type Error = MachineFailure;

    fn reduce(
        &self,
        facts: Vec<polis::VerifiedFact>,
    ) -> std::result::Result<Self::View, Self::Error> {
        let mut machine_facts = Vec::with_capacity(facts.len());
        for fact in facts {
            let decoded = MachineMembershipFact::decode_payload(fact.payload().as_bytes())
                .map_err(map_machine_fact_error)?;
            let expected_target = decoded
                .encode()
                .map_err(map_machine_fact_error)?
                .target()
                .clone();
            let actual_target = super::product_fact_receipt(fact.receipt())
                .map_err(map_machine_primitive)?
                .target()
                .clone();
            if actual_target != expected_target {
                return Err(MachineFailure::InvalidPayload);
            }
            machine_facts.push(decoded);
        }
        self.reducer
            .reduce(machine_facts)
            .map_err(map_machine_fact_error)
    }
}

fn machine_projection_view(machine: &MachineId) -> Result<polis::ProjectionView, MachineFailure> {
    polis::ProjectionKey::parse(format!("ployz.machine.membership:{}", machine.as_str()))
        .map(polis::ProjectionView::new)
        .map_err(map_machine_polis_error)
}

fn machine_fact_query(
    context: &MutationContext,
    machine: &MachineId,
) -> Result<polis::FactQuery, MachineFailure> {
    let scope = polis::ScopeId::parse(context.authority().scope().as_str())
        .map_err(map_machine_polis_error)?;
    let resource = polis::ResourceId::parse(format!("machine:{}", machine.as_str()))
        .map_err(map_machine_polis_error)?;
    Ok(polis::FactQuery::new(scope).resource(resource))
}

struct MachineFactGrantAuthority;

impl polis::FactGrantAuthority for MachineFactGrantAuthority {
    fn decide(
        &self,
        _authority: &polis::AuthorityContext,
        target: &polis::FactTarget,
        purpose: polis::FactGrantPurpose,
    ) -> polis::FactGrantDecision {
        match purpose {
            polis::FactGrantPurpose::Append if machine_fact_target_allowed(target) => {
                polis::FactGrantDecision::allowed()
            }
            polis::FactGrantPurpose::Append | polis::FactGrantPurpose::ReplicaImport => {
                polis::FactGrantDecision::denied()
            }
        }
    }
}

fn machine_fact_target_allowed(target: &polis::FactTarget) -> bool {
    target.resource().as_str().starts_with("machine:")
        && matches!(
            target.kind().as_str(),
            MACHINE_JOINED_KIND | MACHINE_REMOVAL_STARTED_KIND | MACHINE_TOMBSTONED_KIND
        )
}

fn map_machine_projection_error(error: polis::ProjectionError<MachineFailure>) -> MachineFailure {
    match error {
        polis::ProjectionError::Source(source) => map_machine_polis_error(source),
        polis::ProjectionError::SubstrateFailed { .. } => MachineFailure::ProjectionUnavailable,
        polis::ProjectionError::Reducer(failure) => failure,
    }
}

fn map_machine_primitive(failure: PrimitiveFailure) -> MachineFailure {
    match failure {
        PrimitiveFailure::MalformedPayload => MachineFailure::InvalidPayload,
        PrimitiveFailure::Unauthorized
        | PrimitiveFailure::Conflict
        | PrimitiveFailure::Timeout
        | PrimitiveFailure::StaleFence
        | PrimitiveFailure::NoResponder
        | PrimitiveFailure::FreshnessUnknown
        | PrimitiveFailure::OperationStateConflict
        | PrimitiveFailure::OperationAlreadySucceeded
        | PrimitiveFailure::OperationInProgress
        | PrimitiveFailure::OperationAlreadyFailed
        | PrimitiveFailure::OperationInterrupted => MachineFailure::ProjectionUnavailable,
    }
}

fn map_machine_polis_error(error: polis::Error) -> MachineFailure {
    map_machine_primitive(super::map_polis_error(error))
}

fn map_machine_fact_error(error: MachineFactError) -> MachineFailure {
    match error {
        MachineFactError::InvalidPayload | MachineFactError::TargetMismatch => {
            MachineFailure::InvalidPayload
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{
        IrohEndpointId, MachineEpoch, MachineNetworkIdentity, MachineRemoval, MachineRemovalReason,
        MachineTombstone, OverlayIp, WireGuardPublicKey,
    };
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn fact_backed_machine_same_epoch_identity_conflict_projects_conflicted() {
        let membership = PolisMachineMembership::in_memory();
        membership
            .join(&context(), &machine_membership("node-a", 1, "fd00::1"))
            .expect("first join");
        let error = membership
            .join(&context(), &machine_membership("node-a", 1, "fd00::2"))
            .expect_err("conflict");

        assert_eq!(
            error,
            MachineFailure::MembershipConflict {
                machine: MachineId::parse("node-a").expect("machine"),
                epoch: Some(MachineEpoch::new(1).expect("epoch")),
            }
        );
        assert!(matches!(
            membership
                .observe(&context(), &MachineId::parse("node-a").expect("machine"))
                .expect("observe"),
            MachineStatus::Conflicted { machine, epoch }
                if machine.as_str() == "node-a" && epoch.value() == 1
        ));
    }

    #[test]
    fn fact_backed_machine_removal_reason_conflict_still_projects_removing() {
        let membership = PolisMachineMembership::in_memory();
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::joined(machine_membership("node-a", 1, "fd00::1")),
            )
            .expect("joined");
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::removal_started(MachineRemoval {
                    machine: MachineId::parse("node-a").expect("machine"),
                    epoch: MachineEpoch::new(2).expect("epoch"),
                    reason: MachineRemovalReason::parse("graceful").expect("reason"),
                }),
            )
            .expect("removing");
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::removal_started(MachineRemoval {
                    machine: MachineId::parse("node-a").expect("machine"),
                    epoch: MachineEpoch::new(2).expect("epoch"),
                    reason: MachineRemovalReason::parse("operator").expect("reason"),
                }),
            )
            .expect("second removing");

        assert!(matches!(
            membership
                .observe(&context(), &MachineId::parse("node-a").expect("machine"))
                .expect("observe"),
            MachineStatus::Removing(removal)
                if removal.machine.as_str() == "node-a" && removal.epoch.value() == 2
        ));
    }

    #[test]
    fn fact_backed_machine_tombstone_reason_conflict_still_projects_tombstoned() {
        let membership = PolisMachineMembership::in_memory();
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::joined(machine_membership("node-a", 1, "fd00::1")),
            )
            .expect("joined");
        for reason in ["force", "operator"] {
            membership
                .append_machine_fact(
                    &context(),
                    &MachineMembershipFact::tombstoned(
                        MachineTombstone {
                            machine: MachineId::parse("node-a").expect("machine"),
                            epoch: MachineEpoch::new(2).expect("epoch"),
                        },
                        MachineRemovalReason::parse(reason).expect("reason"),
                    ),
                )
                .expect("tombstone");
        }

        assert!(matches!(
            membership
                .observe(&context(), &MachineId::parse("node-a").expect("machine"))
                .expect("observe"),
            MachineStatus::Tombstoned(tombstone)
                if tombstone.machine.as_str() == "node-a" && tombstone.epoch.value() == 2
        ));
    }

    #[test]
    fn degraded_projection_is_not_exposed_as_machine_status() {
        let membership = PolisMachineMembership::in_memory();
        append_conflicting_raw_machine_fact(&membership, "payload-a");
        append_conflicting_raw_machine_fact(&membership, "payload-b");

        assert_eq!(
            membership.observe(&context(), &MachineId::parse("node-a").expect("machine")),
            Err(MachineFailure::ProjectionUnavailable)
        );
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("deploy-1").expect("operation"),
            IdempotencyKey::parse("deploy-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn append_conflicting_raw_machine_fact(
        membership: &PolisMachineMembership<polis::MemoryFactStore>,
        payload: &str,
    ) {
        let target = polis::FactTarget::new(
            polis::ResourceId::parse("machine:node-a").expect("resource"),
            polis::FactKey::parse("/facts/node/node-a/conflict").expect("key"),
            polis::FactKind::parse(MACHINE_JOINED_KIND).expect("kind"),
        );
        let authority = super::super::context_fact_append_authority(&context()).expect("authority");
        let grant = polis::FactGrantService::new(MachineFactGrantAuthority)
            .issue_append(&authority, target.clone())
            .expect("grant");
        let request = polis::FactAppendRequest::new(
            polis::OperationId::parse(format!("raw-machine-{payload}")).expect("operation"),
            polis::IdempotencyKey::parse(format!("raw-machine-{payload}")).expect("idempotency"),
            authority,
            grant,
            target,
            polis::FactPayload::new(payload.as_bytes().to_vec()).expect("payload"),
            None,
        );

        polis::FactStore::append(membership.projection.facts(), request).expect("append");
    }

    fn machine_membership(machine: &str, epoch: u64, overlay_ip: &str) -> MachineMembership {
        MachineMembership::new(
            MachineId::parse(machine).expect("machine"),
            MachineEpoch::new(epoch).expect("epoch"),
            MachineNetworkIdentity::new(
                OverlayIp::parse(overlay_ip).expect("overlay ip"),
                IrohEndpointId::parse(format!("iroh-{machine}")).expect("iroh endpoint"),
                WireGuardPublicKey::parse(format!("wg-{machine}")).expect("wireguard key"),
            ),
        )
    }
}
