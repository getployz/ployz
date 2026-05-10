pub mod participant;

mod execute;
mod lifecycle;
mod managed_domains;
mod plan;
mod probe;

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

pub use execute::DeployApplyPreconditions;
pub use probe::{NoopParticipantProbe, ParticipantProbe, ProbeError, ProbeErrorKind};

pub fn new_deploy_id() -> crate::model::DeployId {
    execute::new_deploy_id()
}

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

pub async fn apply_with_deploy_id_and_certificate_coordination(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
    deploy_id: crate::model::DeployId,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
) -> Result<DeployApplyResult> {
    apply_with_deploy_id_and_preconditions(
        store,
        participant_client,
        local_machine_id,
        manifest,
        deploy_id,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        prober,
        DeployApplyPreconditions::default(),
    )
    .await
}

pub async fn apply_with_deploy_id_and_preconditions(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
    deploy_id: crate::model::DeployId,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
    preconditions: DeployApplyPreconditions<'_>,
) -> Result<DeployApplyResult> {
    execute::apply_with_deploy_id_and_preconditions(
        store,
        participant_client,
        local_machine_id,
        manifest,
        deploy_id,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        prober,
        preconditions,
    )
    .await
}
