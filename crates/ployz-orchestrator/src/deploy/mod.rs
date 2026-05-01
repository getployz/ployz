pub mod participant;

mod execute;
mod managed_domains;
mod plan;
mod probe;
mod transaction;

#[cfg(test)]
mod tests;

use crate::certificates::{
    AcmeAccountCoordinator, AcmeIssuerFactory, Http01ChallengeReadiness, IssuanceCoordinator,
};
use crate::deploy::participant::DeployParticipantClient;
use crate::error::Result;
use crate::model::{DeployApplyResult, DeployPreview, MachineId};
use plan::resolve_plan;
use ployz_store_api::StoreDriver;
use ployz_types::spec::DeployManifest;
use probe::{probe_participants, warnings_from_reachability};
use std::sync::Arc;

pub use probe::{NoopParticipantProbe, ParticipantProbe, ProbeError, ProbeErrorKind};

pub async fn preview(
    store: &StoreDriver,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
    prober: &dyn ParticipantProbe,
) -> Result<DeployPreview> {
    let plan = resolve_plan(store, local_machine_id, manifest).await?;
    managed_domains::validate_hostname_ownership(store, &plan).await?;
    let reachability = probe_participants(prober, plan.participants(), plan.machine_map()).await;
    let mut warnings = warnings_from_reachability(&reachability);
    warnings.extend(managed_domains::warnings_for_plan(store, &plan).await?);
    Ok(plan.to_preview(warnings))
}

pub async fn apply(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployApplyResult> {
    execute::apply(store, participant_client, local_machine_id, manifest).await
}

pub async fn apply_with_certificate_coordination(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
) -> Result<DeployApplyResult> {
    execute::apply_with_certificate_coordination(
        store,
        participant_client,
        local_machine_id,
        manifest,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        prober,
    )
    .await
}
