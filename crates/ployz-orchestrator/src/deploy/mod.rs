pub mod participant;
mod participant_set;
mod volume_execution;

mod execute;
mod lifecycle;
mod managed_domains;
mod plan;
mod probe;

#[cfg(test)]
mod tests;

use crate::deploy::participant::DeployParticipantClient;
use crate::error::{DeployError, Error, Result};
use crate::model::{
    DeployApplyResult, DeployPreview, MachineId, PreparedDeployRecord, PreparedDeployState,
};
use plan::{preflight_image_availability, resolve_plan};
use ployz_cert_api::{
    AcmeAccountCoordinator, AcmeIssuerFactory, Http01ChallengeReadiness, IssuanceCoordinator,
};
use ployz_spec::{DeployManifest, stable_hash_hex};
use ployz_store_api::{DeployStore, StoreDriver};
use ployz_time::now_unix_secs;
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
    resolved_preview(store, local_machine_id, manifest, prober).await
}

async fn resolved_preview(
    store: &StoreDriver,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
    prober: &dyn ParticipantProbe,
) -> Result<DeployPreview> {
    let plan = resolve_plan(store, local_machine_id, manifest).await?;
    managed_domains::validate_hostname_ownership(store, &plan).await?;
    let image_availability = preflight_image_availability(store, &plan).await?;
    let reachability = probe_participants(prober, plan.participants(), plan.machine_map()).await;
    let mut warnings = warnings_from_reachability(&reachability);
    warnings.extend(managed_domains::warnings_for_plan(store, &plan).await?);
    Ok(plan.to_preview_with_image_availability(warnings, image_availability))
}

pub async fn prepare(
    store: &StoreDriver,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
    prober: &dyn ParticipantProbe,
    prepared_deploy_id: crate::model::DeployId,
    ttl_secs: u64,
) -> Result<PreparedDeployRecord> {
    let preview = resolved_preview(store, local_machine_id, manifest, prober).await?;
    let baseline = preview.baseline.clone().ok_or_else(|| {
        crate::error::Error::operation("deploy_prepare", "resolved preview missing baseline")
    })?;
    let manifest_json = serde_json::to_string(manifest).map_err(|error| {
        crate::error::Error::operation("deploy_prepare", format!("serialize manifest: {error}"))
    })?;
    let now = now_unix_secs();
    let record = PreparedDeployRecord {
        prepared_deploy_id,
        namespace: manifest.namespace.clone(),
        manifest_hash: preview.manifest_hash.clone(),
        manifest_json,
        preview,
        baseline,
        coordinator_machine_id: local_machine_id.clone(),
        state: PreparedDeployState::Prepared,
        created_at: now,
        expires_at: now.saturating_add(ttl_secs),
        updated_at: now,
    };
    store.write_prepared_deploy(&record).await?;
    Ok(record)
}

pub fn validated_prepared_manifest(prepared: &PreparedDeployRecord) -> Result<DeployManifest> {
    let manifest: DeployManifest =
        serde_json::from_str(&prepared.manifest_json).map_err(|error| {
            invalid_prepared_record(
                &prepared.prepared_deploy_id,
                format!("stored prepared deploy manifest is invalid: {error}"),
            )
        })?;
    if manifest.namespace != prepared.namespace {
        return Err(invalid_prepared_record(
            &prepared.prepared_deploy_id,
            "stored manifest namespace does not match prepared record namespace",
        ));
    }
    let manifest_hash = stable_hash_hex(
        serde_json::to_vec(&manifest)
            .map_err(|error| {
                Error::operation(
                    "prepared_deploy_validate",
                    format!("serialize manifest: {error}"),
                )
            })?
            .as_slice(),
    );
    if prepared.manifest_hash != manifest_hash || prepared.preview.manifest_hash != manifest_hash {
        return Err(invalid_prepared_record(
            &prepared.prepared_deploy_id,
            "stored manifest hash does not match prepared record hash",
        ));
    }
    if prepared.preview.namespace != prepared.namespace {
        return Err(invalid_prepared_record(
            &prepared.prepared_deploy_id,
            "stored preview namespace does not match prepared record namespace",
        ));
    }
    if prepared.preview.baseline.as_ref() != Some(&prepared.baseline) {
        return Err(invalid_prepared_record(
            &prepared.prepared_deploy_id,
            "stored preview baseline does not match prepared record baseline",
        ));
    }
    if !prepared.baseline.is_canonical() {
        return Err(invalid_prepared_record(
            &prepared.prepared_deploy_id,
            "stored prepared deploy baseline fingerprint must match components",
        ));
    }
    Ok(manifest)
}

fn invalid_prepared_record(
    prepared_deploy_id: &crate::model::DeployId,
    message: impl Into<String>,
) -> Error {
    Error::Deploy(DeployError::PreparedDeployInvalid {
        prepared_deploy_id: prepared_deploy_id.as_str().to_string(),
        message: message.into(),
    })
}

pub async fn apply(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployApplyResult> {
    execute::apply(store, participant_client, local_machine_id, manifest).await
}

pub async fn apply_prepared_with_certificate_coordination(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    prepared_deploy_id: crate::model::DeployId,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
) -> Result<DeployApplyResult> {
    execute::apply_prepared_with_certificate_coordination(
        store,
        participant_client,
        local_machine_id,
        prepared_deploy_id,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        prober,
    )
    .await
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
    preconditions: DeployApplyPreconditions,
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
