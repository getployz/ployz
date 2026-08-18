//! The Deploy pipeline: snapshot → plan → execute.
//!
//! A Deploy Snapshot is gathered, a Deploy Preview is calculated, and a Deploy
//! Plan is executed to a Deploy Outcome. This module does not print, read
//! stdin, or exit the process.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::SystemTime;

use ployz_core::{
    DataLoss, MachineFailure, MachineId, MachineObservation, ObservedDataLoss, PortPublication,
    ProjectName, RequestedServiceSpec, RpcError, RpcErrorCode, ServiceMode, ServiceSelector,
    UnconfirmedDataLoss, select_service,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{BuildService, ComposeProject},
    connect::{Client, ConnectError},
    dns::{ExpandIngressError, IngressDnsWarning, resolve_ingress_dns_warnings},
    failure::Failure,
    image::PushError,
};

use super::{
    ComposePruneRefusal, DeployEvent, DeployIntent, DeployOutcome, DeployPlan, DeployPreview,
    DeploySnapshot, DeployWarning, ExecutionError, ObservationKind, PlanError, PlanOptions,
    exec::execute_operation_sequence, pending_rows, planning,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReconciliationHints {
    pub requested_profiles: Vec<String>,
    pub compose_refusal: Option<ComposePruneRefusal>,
}

/// Snapshot, planning, or ingress-expansion failure before a Deploy executes.
///
/// Execution failure is a [`DeployOutcome::Failed`], not this error.
#[derive(Debug, Error)]
pub enum DeployError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Ingress(#[from] ExpandIngressError),
    #[error(transparent)]
    Project(#[from] crate::project::ProjectError),
}

impl Client {
    /// Calculate a Deploy Preview for a Deploy Intent without executing it.
    ///
    /// Reuses the same planner, ingress expansion, and DNS warnings as the CLI.
    /// Confirming executes these operations; it does not re-plan.
    ///
    /// # Errors
    ///
    /// Returns when snapshot gathering, ingress expansion, or planning fails.
    pub async fn preview(
        &mut self,
        mut intent: DeployIntent,
    ) -> Result<DeployPreview, DeployError> {
        let machines = self.machines().await?;
        let (snapshot, warnings) = gather_snapshot(self, machines).await?;
        prepare_intent(self, snapshot, warnings, &mut intent).await
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
    ) -> Result<DeployPreview, DeployError> {
        let removal = self.plan_project_destroy(project, volumes).await?;
        Ok(removal.preview(project))
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
        let removal = self.plan_project_destroy(project, volumes).await?;
        observed_destroy_loss(&removal.plan, volumes)
    }

    /// Re-read Data Loss, refuse when uncovered, then plan and execute removal.
    ///
    /// Confirmation is [`ObservedDataLoss::uncovered_by`]. Extra names are
    /// ignored. Executes the execute-time plan; it does not replay an earlier
    /// preview.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Project is reserved or not
    /// visible, the snapshot is incomplete, or the confirmation does not cover
    /// the fresh Data Loss. Execution failure is a [`DeployOutcome::Failed`].
    pub async fn destroy_project(
        &mut self,
        project: &ProjectName,
        confirm_data_loss: &[DataLoss],
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
        confirm_data_loss: &[DataLoss],
        volumes: super::VolumeFate,
    ) -> Result<DeployPreview, RpcError> {
        let removal = self.plan_project_destroy(project, volumes).await?;
        let missing =
            observed_destroy_loss(&removal.plan, volumes)?.uncovered_by(confirm_data_loss);
        if !missing.is_empty() {
            return Err(UnconfirmedDataLoss { missing }.into_rpc_error());
        }
        let preview = removal.preview(project);
        if let Some(reason) = preview.prune_refusal {
            return Err(invalid_argument(reason.to_string()));
        }
        if project_not_found(&preview) {
            return Err(RpcError {
                code: RpcErrorCode::NotFound,
                message: format!("Project '{project}' was not found"),
                details: serde_json::Value::Null,
            });
        }
        Ok(preview)
    }

    async fn plan_project_destroy(
        &mut self,
        project: &ProjectName,
        volumes: super::VolumeFate,
    ) -> Result<ProjectRemoval, DeployError> {
        crate::project::refuse_reserved(project)?;
        let machines = self.machines().await?;
        let (snapshot, warnings) = gather_snapshot(self, machines).await?;
        let plan = planning::plan_project_removal(project, &snapshot, volumes)?;
        Ok(ProjectRemoval {
            snapshot,
            warnings,
            plan,
        })
    }

    /// Execute the operations on this Deploy Preview. Does not re-plan.
    ///
    /// Progress events are sent on `progress` when provided. The first event is
    /// every row `pending` before any Machine RPC. Execution failure is a
    /// [`DeployOutcome::Failed`], not this error.
    pub async fn confirm(
        &self,
        preview: &DeployPreview,
        cancellation: &CancellationToken,
        progress: Option<tokio::sync::mpsc::UnboundedSender<DeployEvent>>,
    ) -> DeployOutcome<ExecutionError> {
        execute_operation_sequence(
            preview.operations.clone(),
            self,
            cancellation,
            progress,
            &preview.project_name,
        )
        .await
    }

    /// Preview, auto-confirm, and return the Deploy Outcome.
    ///
    /// # Errors
    ///
    /// Returns when snapshot gathering, ingress expansion, or planning fails
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

struct ProjectRemoval {
    snapshot: DeploySnapshot,
    warnings: Vec<DeployWarning>,
    plan: DeployPlan,
}

impl ProjectRemoval {
    fn preview(self, project: &ProjectName) -> DeployPreview {
        DeployPreview {
            project_name: project.clone(),
            operations: pending_rows(&self.plan.operations, &self.snapshot),
            warnings: self.warnings,
            would_remove: self.plan.would_remove,
            preserved_volumes: self.plan.preserved_volumes,
            prune_refusal: self.plan.prune_refusal,
        }
    }
}

fn observed_destroy_loss(
    plan: &DeployPlan,
    volumes: super::VolumeFate,
) -> Result<ObservedDataLoss, RpcError> {
    if volumes == super::VolumeFate::Destroy
        && let Some(reason) = plan.prune_refusal
    {
        return Err(invalid_argument(reason.to_string()));
    }
    Ok(planning::data_loss_from_plan(plan))
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
            DeployError::Plan(error) => invalid_argument(error.to_string()),
            DeployError::Ingress(error) => invalid_argument(error.to_string()),
            DeployError::Project(error) => invalid_argument(error.to_string()),
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
        Self::IngressHostname(warning.to_string())
    }
}

pub(super) async fn plan_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    options: PlanOptions,
    project_name: &ProjectName,
) -> Result<DeployPreview, Failure> {
    let machines = list_machines(client).await?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    let mut intent = DeployIntent::apply_one(project_name.clone(), requested.clone(), options);
    Ok(prepare_intent(client, snapshot, warnings, &mut intent).await?)
}

pub(super) async fn push_project_images(
    client: &mut Client,
    builds: &[BuildService],
    machines: &[MachineObservation],
) -> Result<PushOutcome, Failure> {
    let mut pushed = Vec::new();
    let mut failures = Vec::new();
    for service in builds {
        match push_image(client, service, machines).await {
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
    project: &mut ComposeProject,
    machines: Vec<MachineObservation>,
    options: PlanOptions,
    project_name: &ProjectName,
    hints: ReconciliationHints,
) -> Result<DeployPreview, Failure> {
    project.resolve_secrets()?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    super::reject_missing_external_volumes(project, &snapshot)?;
    let mut intent = DeployIntent::from_named_specs(
        project_name.clone(),
        &project.services,
        &project.dependencies,
        options,
    )
    .with_service_profiles(project.service_profiles())
    .with_requested_profiles(hints.requested_profiles)
    .with_compose_refusal(hints.compose_refusal);
    Ok(prepare_intent(client, snapshot, warnings, &mut intent).await?)
}

pub(super) async fn plan_scale(
    client: &mut Client,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
    options: PlanOptions,
) -> Result<(DeployPreview, ProjectName), Failure> {
    let machines = list_machines(client).await?;
    let (snapshot, warnings) = gather_snapshot(client, machines).await?;
    let choice = choose_scale_spec(&snapshot, selector, replicas)?;
    let Some(requested) = choice.requested else {
        return Ok((
            DeployPreview::new(Vec::new(), warnings, choice.project_name.clone()),
            choice.project_name,
        ));
    };
    let mut intent = DeployIntent::apply_one(choice.project_name.clone(), requested, options);
    Ok((
        prepare_intent(client, snapshot, warnings, &mut intent).await?,
        choice.project_name,
    ))
}

async fn prepare_intent(
    client: &mut Client,
    snapshot: DeploySnapshot,
    mut warnings: Vec<DeployWarning>,
    intent: &mut DeployIntent,
) -> Result<DeployPreview, DeployError> {
    expand_ingress(client, &intent.project_name, intent.target.iter_mut()).await?;
    warnings.extend(hostname_warnings(intent.target.iter(), &snapshot.machines).await);
    let plan = planning::plan_deploy(intent, &snapshot)?;
    warnings.extend(plan.warnings);
    Ok(DeployPreview {
        project_name: intent.project_name.clone(),
        operations: pending_rows(&plan.operations, &snapshot),
        warnings,
        would_remove: plan.would_remove,
        preserved_volumes: plan.preserved_volumes,
        prune_refusal: plan.prune_refusal,
    })
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
    // TODO(UT-046): mixed historical specs use one observed regular container; there is no chooser.
    let mut requested = observed_container.resolved_spec.to_requested();
    requested.mode = ServiceMode::Replicated { replicas };
    Ok(ScaleSpec {
        project_name,
        requested: Some(requested),
    })
}

pub(super) async fn list_machines(client: &mut Client) -> Result<Vec<MachineObservation>, Failure> {
    Ok(client.machines().await?)
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
    warnings.extend(observation_warnings(
        ObservationKind::Volume,
        &snapshot.volume_failures,
        &snapshot.volume_omissions,
    ));
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

async fn expand_ingress<'a>(
    client: &mut Client,
    project: &ProjectName,
    specs: impl IntoIterator<Item = &'a mut RequestedServiceSpec>,
) -> Result<(), DeployError> {
    let specs: Vec<_> = specs.into_iter().collect();
    if !specs.iter().any(|spec| needs_ingress_expansion(spec)) {
        return Ok(());
    }
    let domain = client.domain_if_reserved().await?;
    for spec in specs {
        crate::dns::expand_ingress_ports(spec, project, domain.as_deref())?;
    }
    Ok(())
}

async fn hostname_warnings<'a>(
    specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    machines: &[MachineObservation],
) -> Vec<DeployWarning> {
    resolve_ingress_dns_warnings(specs, &machine_public_addresses(machines))
        .await
        .into_iter()
        .map(DeployWarning::from)
        .collect()
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
    service: &BuildService,
    machines: &[MachineObservation],
) -> Result<(Vec<PushedImage>, Vec<String>), PushError> {
    let targets = service
        .machines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let result =
        crate::image::push_using_machines(client, &service.image, None, &targets, machines).await?;
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
