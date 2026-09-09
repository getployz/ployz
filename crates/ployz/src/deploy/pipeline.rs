//! The Deploy pipeline: snapshot → preview → confirm.
//!
//! A Deploy Snapshot is gathered, a Deploy Preview is calculated, and
//! `confirm` executes the privately admitted plan to a Deploy Outcome. This module does not
//! print, read stdin, or exit the process.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::SystemTime;

use ployz_core::{
    DataLossConfirmation, MachineFailure, MachineId, MachineObservation, ObservedDataLoss,
    PortPublication, ProjectName, RequestedServiceSpec, RpcError, RpcErrorCode, ServiceMode,
    ServiceSelector, UnconfirmedDataLoss, select_service,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{BuiltService, CapturedCompose},
    connect::{Client, ConnectError},
    dns::{IngressDnsWarning, resolve_ingress_dns_warnings_for_ports},
    failure::Failure,
    image::PushError,
};

use super::{
    ComposePruneRefusal, DeployEvent, DeployIntent, DeployOutcome, DeployPlan, DeployPreview,
    DeploySnapshot, DeployWarning, ExecutionError, IngressContext, ObservationKind, PlanError,
    PlanOptions, exec::execute_operation_sequence, plan_deploy, planning,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReconciliationHints {
    pub requested_profiles: Vec<String>,
    pub compose_refusal: Option<ComposePruneRefusal>,
}

/// Snapshot or planning failure before a Deploy executes.
///
/// Execution failure is a [`DeployOutcome::Failed`], not this error.
#[derive(Debug, Error)]
pub enum DeployError {
    #[error("{0}. No Service, hook, or volume change was attempted.")]
    Build(#[from] crate::compose::ComposeError),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Project(#[from] crate::project::ProjectError),
}

impl Client {
    /// Calculate a Deploy Preview for a Deploy Intent without executing it.
    ///
    /// Gathers the observer-relative snapshot and reserved domain, calls
    /// [`plan_deploy`], then performs public-DNS warning I/O. Confirming
    /// executes these operations; it does not re-plan.
    ///
    /// # Errors
    ///
    /// Returns when snapshot gathering or planning fails.
    pub async fn preview(&mut self, intent: DeployIntent) -> Result<DeployPlan, DeployError> {
        let machines = self.machines().await?;
        let (snapshot, warnings) = gather_deploy_snapshot(self, machines, &intent).await?;
        preview_gathered(self, snapshot, warnings, &intent).await
    }

    /// Calculate a Project-removal preview. Confirming executes these operations.
    ///
    /// Reserved names reuse [`crate::project::refuse_reserved`]. Incomplete
    /// snapshots reuse [`DeployIntent::prune_refusal`].
    ///
    /// # Errors
    ///
    /// Returns when the Project is reserved or snapshot gathering or planning fails.
    pub async fn preview_project_removal(
        &mut self,
        project: &ProjectName,
        volumes: super::VolumeFate,
    ) -> Result<DeployPlan, DeployError> {
        crate::project::refuse_reserved(project)?;
        let machines = self.machines().await?;
        let (snapshot, warnings) = gather_snapshot(self, machines).await?;
        let mut preview = planning::prepare_project_removal(project, &snapshot, volumes)?;
        preview.prepend_warnings(warnings);
        Ok(preview)
    }

    /// Live Observation of Data Loss that destroying `project` would cause.
    ///
    /// [`super::VolumeFate::Preserve`] yields an empty list. Mutates nothing.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Project is reserved, snapshot
    /// gathering fails, or destroying volumes is requested against a known
    /// incomplete snapshot.
    pub async fn data_loss_if_project_destroyed(
        &mut self,
        project: &ProjectName,
        volumes: super::VolumeFate,
    ) -> Result<ObservedDataLoss, RpcError> {
        let preview = self.preview_project_removal(project, volumes).await?;
        require_project_present(&preview)?;
        observed_destroy_loss(&preview, volumes)
    }

    /// Re-read Data Loss, refuse when uncovered, then plan and execute removal.
    ///
    /// Confirmation is checked with [`ObservedDataLoss::require`]. Confirmed
    /// identities that disappeared are ignored. Executes the execute-time
    /// plan; it does not replay an earlier preview.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Project is reserved or not
    /// visible, the snapshot is incomplete, or the confirmation does not cover
    /// the fresh Data Loss. Execution failure is a [`DeployOutcome::Failed`].
    pub async fn destroy_project(
        &mut self,
        project: &ProjectName,
        confirm_data_loss: &DataLossConfirmation,
        volumes: super::VolumeFate,
        cancellation: &CancellationToken,
        progress: Option<tokio::sync::mpsc::UnboundedSender<DeployEvent>>,
    ) -> Result<DeployOutcome<ExecutionError>, RpcError> {
        let preview = self
            .prepare_project_destroy(project, confirm_data_loss, volumes)
            .await?;
        Ok(self.confirm(&preview, cancellation, progress).await)
    }

    /// Execute-time re-read and plan. Confirming runs these operations.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Project is reserved or not
    /// visible, the snapshot is incomplete, planning fails, or the confirmation
    /// does not cover the fresh Data Loss.
    pub(crate) async fn prepare_project_destroy(
        &mut self,
        project: &ProjectName,
        confirm_data_loss: &DataLossConfirmation,
        volumes: super::VolumeFate,
    ) -> Result<DeployPlan, RpcError> {
        let preview = self.preview_project_removal(project, volumes).await?;
        require_project_present(&preview)?;
        observed_destroy_loss(&preview, volumes)?
            .require(confirm_data_loss)
            .map_err(UnconfirmedDataLoss::into_rpc_error)?;
        if let Some(reason) = preview.prune_refusal {
            return Err(invalid_argument(format!(
                "{reason}: {}",
                preview
                    .warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }

        Ok(preview)
    }

    /// Execute this admitted Deploy Plan. Does not re-plan.
    ///
    /// Progress events are sent on `progress` when provided. The first event is
    /// every row `pending` before any Machine RPC. Execution failure is a
    /// [`DeployOutcome::Failed`], not this error.
    pub async fn confirm(
        &self,
        preview: &DeployPlan,
        cancellation: &CancellationToken,
        progress: Option<tokio::sync::mpsc::UnboundedSender<DeployEvent>>,
    ) -> DeployOutcome<ExecutionError> {
        execute_operation_sequence(preview, self, cancellation, progress).await
    }

    /// Preview, auto-confirm, and return the Deploy Outcome.
    ///
    /// # Errors
    ///
    /// Returns when snapshot gathering or planning fails
    /// before execution starts.
    pub async fn run(
        &mut self,
        intent: DeployIntent,
        cancellation: &CancellationToken,
        progress: Option<tokio::sync::mpsc::UnboundedSender<DeployEvent>>,
    ) -> Result<DeployOutcome<ExecutionError>, DeployError> {
        let preview = self.preview(intent).await?;
        Ok(self.confirm(&preview, cancellation, progress).await)
    }
}

fn observed_destroy_loss(
    preview: &DeployPreview,
    volumes: super::VolumeFate,
) -> Result<ObservedDataLoss, RpcError> {
    if volumes == super::VolumeFate::Destroy
        && let Some(reason) = preview.prune_refusal
    {
        return Err(invalid_argument(format!(
            "{reason}: {}",
            preview
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    Ok(planning::data_loss_from_plan(preview))
}

fn require_project_present(preview: &DeployPreview) -> Result<(), RpcError> {
    if project_not_found(preview) {
        return Err(RpcError {
            code: RpcErrorCode::NotFound,
            message: format!(
                "Project '{}' was not found in this Cluster observation. No changes made.",
                preview.project_name
            ),
            details: serde_json::Value::Null,
        });
    }
    Ok(())
}

pub(crate) fn project_not_found(preview: &DeployPreview) -> bool {
    preview.prune_refusal.is_none()
        && preview.operations.is_empty()
        && preview.preserved_volumes.is_empty()
        && preview.would_remove.is_empty()
}

impl From<DeployError> for RpcError {
    fn from(error: DeployError) -> Self {
        match error {
            DeployError::Connect(error) => error.into(),
            DeployError::Plan(error) => error.into_rpc_error(),
            DeployError::Project(error) => invalid_argument(error.to_string()),
            DeployError::Build(error) => invalid_argument(error.to_string()),
        }
    }
}

fn invalid_argument(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidArgument,
        message,
        details: serde_json::Value::Null,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PushOutcome {
    pub pushed: Vec<PushedImage>,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PushedImage {
    pub image: String,
    pub machine_id: MachineId,
}

impl From<IngressDnsWarning> for DeployWarning {
    fn from(warning: IngressDnsWarning) -> Self {
        Self::IngressHostname {
            message: warning.to_string(),
        }
    }
}

pub(super) async fn push_project_images(
    client: &mut Client,
    builds: &[BuiltService],
    machines: &[MachineObservation],
    preview: &DeployPlan,
    cancellation: &CancellationToken,
) -> Result<PushOutcome, Failure> {
    let mut pushed = Vec::new();
    let mut failures = Vec::new();
    // Check every actual destination before any image or application changes.
    let deliveries = builds.iter().map(|service| {
        let targets = preview.operations.iter()
            .filter(|row| row.operation.spec().is_some_and(|spec| spec.name.as_str() == service.name))
            .map(|row| row.machine_id).collect::<BTreeSet<_>>();
        for target in &targets {
            let architecture = machines.iter().find(|machine| machine.machine.id == *target)
                .map(|machine| machine.machine.runtime.architecture.as_str());
            let compatible = architecture.is_some_and(|architecture|
                service.built.platforms.iter().any(|platform| crate::image::platform_compatible(platform, architecture)));
            if !compatible {
                return Err(Failure::usage(format!("Build for Service {} contains {}; destination Machine {target} reports architecture {}. No Service, hook, or volume change was attempted.", service.name, service.built.platforms.join(", "), architecture.unwrap_or("unknown"))));
            }
        }
        Ok((service, targets.into_iter().map(|target| target.to_string()).collect::<Vec<_>>()))
    }).collect::<Result<Vec<_>, Failure>>()?;
    for (service, targets) in deliveries {
        if targets.is_empty() {
            continue;
        }
        match push_image(client, service, machines, &targets, cancellation).await {
            Ok((images, service_failures)) => {
                pushed.extend(images);
                failures.extend(service_failures);
            }
            Err(error) => failures.push(format!("{}: {error}", service.image)),
        }
    }
    Ok(PushOutcome { pushed, failures })
}

pub(super) async fn plan_project(
    client: &mut Client,
    candidate: &CapturedCompose,
    machines: Vec<MachineObservation>,
) -> Result<DeployPlan, Failure> {
    let intent = candidate.intent();
    let (snapshot, warnings) = gather_deploy_snapshot(client, machines, intent).await?;
    Ok(preview_gathered(client, snapshot, warnings, intent).await?)
}

pub(super) async fn plan_scale(
    client: &mut Client,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
    options: PlanOptions,
) -> Result<(DeployPlan, ProjectName), Failure> {
    let machines = client.machines().await?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    let choice = choose_scale_spec(&snapshot, selector, replicas)?;
    let Some(requested) = choice.requested else {
        return Ok((
            DeployPlan::empty(choice.project_name.clone(), warnings),
            choice.project_name,
        ));
    };
    let intent = DeployIntent::apply_one(choice.project_name.clone(), requested, options);
    let (snapshot, warnings) = if intent
        .target
        .iter()
        .any(|spec| spec.volume_graph().has_mounted_provisioned_volume())
    {
        gather_deploy_snapshot(client, snapshot.machines, &intent).await?
    } else {
        (snapshot, warnings)
    };
    Ok((
        preview_gathered(client, snapshot, warnings, &intent).await?,
        choice.project_name,
    ))
}

async fn preview_gathered(
    client: &mut Client,
    snapshot: DeploySnapshot,
    mut warnings: Vec<DeployWarning>,
    intent: &DeployIntent,
) -> Result<DeployPlan, DeployError> {
    let domain = if intent.target.iter().any(needs_ingress_expansion) {
        client.domain_if_reserved().await?
    } else {
        None
    };
    let mut preview = plan_deploy(
        intent,
        &snapshot,
        IngressContext {
            cluster_domain: domain.as_deref(),
        },
    )?;
    warnings.extend(hostname_warnings(&preview, &snapshot.machines).await);
    preview.prepend_warnings(warnings);
    Ok(preview)
}

pub(crate) fn plan_options(force_recreate: bool, skip_health_monitor: bool) -> PlanOptions {
    PlanOptions {
        force_recreate,
        skip_health_monitor,
        placement_seed: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64),
        ..PlanOptions::default()
    }
}

#[derive(Debug)]
struct ScaleSpec {
    project_name: ProjectName,
    requested: Option<RequestedServiceSpec>,
}

fn choose_scale_spec(
    snapshot: &DeploySnapshot,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
) -> Result<ScaleSpec, Failure> {
    let services = ployz_core::derive_services(snapshot.containers.iter().cloned());
    let service = select_service(&services, selector)?;
    let observed_container = service
        .containers
        .first()
        .ok_or_else(|| Failure::usage("cannot scale a service without regular containers"))?
        .as_observation();
    match observed_container.resolved_spec.mode {
        ServiceMode::Replicated { .. } => {}
        ServiceMode::Global => return Err(Failure::usage("global services cannot be scaled")),
    }
    let project_name = service.identity.project.clone();
    if usize::try_from(replicas.get()) == Ok(service.containers.len()) {
        return Ok(ScaleSpec {
            project_name,
            requested: None,
        });
    }
    // TODO: mixed historical specs use one observed regular container; there is no chooser.
    let mut requested = observed_container.resolved_spec.to_requested();
    requested.mode = ServiceMode::Replicated { replicas };
    Ok(ScaleSpec {
        project_name,
        requested: Some(requested),
    })
}

async fn gather_snapshot(
    client: &mut Client,
    machines: Vec<MachineObservation>,
) -> Result<(DeploySnapshot, Vec<DeployWarning>), DeployError> {
    let snapshot = client.deploy_snapshot(machines).await?;
    let mut warnings = observation_warnings(
        ObservationKind::Container,
        &snapshot.container_failures,
        &snapshot.container_omissions,
    );
    warnings.extend(snapshot.volume_snapshot.deploy_warnings());
    Ok((snapshot, warnings))
}

async fn gather_deploy_snapshot(
    client: &mut Client,
    mut machines: Vec<MachineObservation>,
    intent: &DeployIntent,
) -> Result<(DeploySnapshot, Vec<DeployWarning>), DeployError> {
    // Import recovery must precede Docker volume reads, whose plugin Get needs the Pool.
    let mut storage_capacity = std::collections::BTreeMap::new();
    if intent
        .target
        .iter()
        .any(|spec| spec.volume_graph().has_mounted_provisioned_volume())
    {
        let mut reads = tokio::task::JoinSet::new();
        for machine in machines
            .iter()
            .filter(|machine| machine.membership.invites_rpc())
        {
            let mut client = client.clone();
            let id = machine.machine.id;
            reads.spawn(async move {
                let result = client
                    .read::<ployz_core::op::InspectStorage>(
                        ployz_core::InspectStorageRequest {},
                        &ployz_core::MachineTarget::from(&id),
                    )
                    .await;
                (id, result)
            });
        }
        while let Some(result) = reads.join_next().await {
            let (id, capacity) = result.expect("storage observation task does not panic");
            storage_capacity.insert(id, capacity);
        }
        client.observe_machine_storage(&mut machines).await;
    }
    let (mut snapshot, warnings) = gather_snapshot(client, machines).await?;
    snapshot.storage_capacity = storage_capacity;
    Ok((snapshot, warnings))
}

fn observation_warnings(
    kind: ObservationKind,
    failures: &[MachineFailure<RpcError>],
    omissions: &[MachineId],
) -> Vec<DeployWarning> {
    failures
        .iter()
        .map(|failure| DeployWarning::ObservationFailed {
            kind,
            machine_id: failure.machine_id,
            message: failure.error.message.clone(),
        })
        .chain(
            omissions
                .iter()
                .map(|machine| DeployWarning::ObservationOmitted {
                    kind,
                    machine_id: *machine,
                }),
        )
        .collect()
}

async fn hostname_warnings(
    preview: &DeployPreview,
    machines: &[MachineObservation],
) -> Vec<DeployWarning> {
    resolve_ingress_dns_warnings_for_ports(
        preview_ports(preview),
        &machine_public_addresses(machines),
    )
    .await
    .into_iter()
    .map(DeployWarning::from)
    .collect()
}

fn preview_ports(preview: &DeployPreview) -> impl Iterator<Item = &PortPublication> {
    preview
        .operations
        .iter()
        .filter_map(|row| row.operation.spec())
        .flat_map(|spec| &spec.ports)
}

fn machine_public_addresses(machines: &[MachineObservation]) -> Vec<IpAddr> {
    machines
        .iter()
        .filter_map(|machine| machine.machine.public_ip)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn needs_ingress_expansion(requested: &RequestedServiceSpec) -> bool {
    requested
        .ports
        .iter()
        .any(|port| matches!(port, PortPublication::Ingress { .. }))
}

async fn push_image(
    client: &mut Client,
    service: &BuiltService,
    machines: &[MachineObservation],
    targets: &[String],
    cancellation: &CancellationToken,
) -> Result<(Vec<PushedImage>, Vec<String>), PushError> {
    let result = match service.location {
        crate::compose::BuildLocation::Local => {
            // Retain each digest even when multiple Services requested one tag.
            let tag = service
                .built
                .repository_reference()
                .map_err(|error| crate::image::PushError::InvalidReference {
                    reference: service.built.reference.clone(),
                    message: error.to_string(),
                })?
                .replace("@sha256:", ":ployz-sha256-");
            crate::image::push_using_machines(
                client,
                crate::image::ImageContent::built(&tag, &service.built.reference),
                None,
                targets,
                machines,
                cancellation,
            )
            .await?
        }
        crate::compose::BuildLocation::Machine(source) => {
            crate::image::push_from_machine_using_machines(
                client,
                &service.built,
                source,
                targets,
                machines,
                cancellation,
            )
            .await?
        }
    };
    let pushed = result
        .successes
        .iter()
        .map(|success| PushedImage {
            image: service.image.clone(),
            machine_id: success.machine_id,
        })
        .collect();
    let failures = result
        .failures
        .into_iter()
        .map(|failure| {
            format!(
                "{} on {}: {}",
                service.image, failure.machine_id, failure.error
            )
        })
        .chain(
            result
                .omissions
                .into_iter()
                .map(|machine| format!("{} on {machine}: no terminal response", service.image)),
        )
        .collect();
    Ok((pushed, failures))
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
