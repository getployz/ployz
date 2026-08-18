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
    MachineFailure, MachineId, MachineObservation, PortPublication, ProjectName,
    RequestedServiceSpec, RpcError, ServiceMode, ServiceSelector, select_service,
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
    ComposePruneRefusal, DeployEvent, DeployIntent, DeployOutcome, DeployPreview, DeploySnapshot,
    DeployWarning, ExecutionError, ObservationKind, PlanError, PlanOptions,
    exec::execute_operation_sequence, pending_rows, plan_deploy,
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
    let plan = plan_deploy(intent, &snapshot)?;
    Ok(DeployPreview {
        project_name: intent.project_name.clone(),
        operations: pending_rows(&plan.operations, &snapshot),
        warnings,
        would_remove: plan.would_remove,
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
