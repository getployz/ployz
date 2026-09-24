//! Cloud preparation: builder eligibility, Builds, planning, and image delivery.
use crate::connect::ConnectError;
use ployz_core::{DescribeContractRequest, MachineTarget, RpcError, RpcErrorCode, op};
use std::time::Duration;

/// Selected builder and rejected observations, for caller-owned presentation.
#[derive(serde::Serialize)]
pub struct SelectedBuilder {
    pub machine: ployz_core::Machine,
    pub rejections: Vec<String>,
}

/// Select the first capable observed Machine, in random order.
/// # Errors
/// Returns exhausted eligibility evidence.
pub async fn select_build_machine(
    client: &mut crate::connect::Client,
    targets: &[ployz_build::Target],
    visible: &[ployz_core::MachineObservation],
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<SelectedBuilder, ConnectError> {
    let mut candidates = visible.iter().collect::<Vec<_>>();
    candidates.sort_by_cached_key(|_| uuid::Uuid::new_v4());
    let mut reasons = Vec::new();
    let mut rejected = std::collections::BTreeMap::<&str, usize>::new();
    for observed in candidates {
        if cancellation.is_cancelled() {
            reasons.push("Build selection cancelled".into());
            break;
        }
        let machine = &observed.machine;
        let category = if !observed.membership.invites_rpc() {
            "membership unavailable"
        } else if !machine.accepts_builds {
            "builds disabled"
        } else {
            "capability unverified"
        };
        let reason = if !observed.membership.invites_rpc() {
            format!("membership is {:?}", observed.membership)
        } else if !machine.accepts_builds {
            "does not accept Builds".into()
        } else {
            match client
                .invoke::<op::DescribeContract>(
                    DescribeContractRequest {},
                    &MachineTarget::from(&machine.id),
                    Some(Duration::from_secs(5)),
                )
                .await
            {
                Ok(contract) if contract.machine_id != machine.id => format!(
                    "contract identifies a different Machine ({})",
                    contract.machine_id
                ),
                Ok(contract) if !contract.supports(ployz_core::BUILD_CAPABILITY) => {
                    "does not support remote Builds".into()
                }
                Ok(_) => match client
                    .check_build_capabilities(machine.id, targets, cancellation)
                    .await
                {
                    Ok(()) => {
                        return Ok(SelectedBuilder {
                            machine: machine.clone(),
                            rejections: reasons,
                        });
                    }
                    Err(error) => format!("Build capability could not be verified: {error}"),
                },
                Err(error) => format!("Build capability could not be verified: {error}"),
            }
        };
        *rejected.entry(category).or_default() += 1;
        reasons.push(format!(
            "Machine {} ({}): {reason}",
            machine.name, machine.id
        ));
    }
    Err(ConnectError::Remote(RpcError {
        code: RpcErrorCode::Unsupported,
        message: format!(
            "no eligible Build Machine: {}",
            if reasons.is_empty() {
                "no Machines observed".into()
            } else {
                reasons.join("; ")
            }
        ),
        details: serde_json::json!({"rejections": rejected}),
    }))
}

use crate::{
    build::{BuiltService, CapturedBuild},
    connect::Client,
    deploy::{DeployError, DeployPlan},
};
use ployz_core::DeployIntent;
use tokio_util::sync::CancellationToken;

/// Preparation failures never imply application confirmation.
#[derive(Debug, thiserror::Error)]
pub enum PreparationError {
    #[error("{0}. No Service, hook, or volume change was attempted.")]
    Build(#[from] crate::build::Error),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error("Build selection failed: {0}")]
    Selection(ConnectError),
    #[error(transparent)]
    Plan(#[from] DeployError),
    #[error("{0}")]
    Delivery(String),
    #[error("preparation cancelled; no application changes attempted")]
    Cancelled,
}

/// Caller-rendered preparation progress; no terminal or process ownership.
#[derive(serde::Serialize)]
pub enum Progress {
    Platforms(Vec<String>),
    Selected(Box<SelectedBuilder>),
    Build(ployz_build::Progress),
    Transfer,
    Delivered {
        image: String,
        machine_id: ployz_core::MachineId,
    },
}

/// Prepared plan with image response streams retained until its owner is dropped.
pub struct Prepared {
    pub plan: DeployPlan,
    builds: Vec<BuiltService>,
}

impl Prepared {
    /// Move plan and retained image owners together into a prepared/running handle.
    #[must_use]
    pub fn into_parts(self) -> (DeployPlan, Vec<BuiltService>) {
        (self.plan, self.builds)
    }
}

/// Build captured source, bind verified images, plan fresh destinations and deliver.
/// # Errors
/// Returns typed Build evidence, eligibility, planning, cancellation or delivery failures.
pub async fn prepare(
    client: &mut Client,
    mut intent: DeployIntent,
    mut build: CapturedBuild,
    reusable: &[BuiltService],
    cancellation: &CancellationToken,
    progress: impl Fn(Progress),
) -> Result<Prepared, PreparationError> {
    let mut builds = Vec::new();
    if build.targets().next().is_some() {
        let mut machines = read(cancellation, async { Ok(client.machines().await?) }).await?;
        let applied = intent.applied_names();
        if intent.target.iter().any(|spec| {
            applied.contains(&spec.name) && spec.volume_graph().has_mounted_provisioned_volume()
        }) {
            read(cancellation, async {
                client.observe_machine_storage(&mut machines).await;
                Ok(())
            })
            .await?;
        }
        build.cover_machines(&intent, &machines)?;
        builds = build
            .reuse_images(client, &intent, &machines, reusable, cancellation)
            .await;
        let targets = build.to_targets();
        let platforms = targets
            .iter()
            .flat_map(|target| target.platforms.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        progress(Progress::Platforms(platforms.into_iter().collect()));
        if !targets.is_empty() {
            let selected = read(cancellation, async {
                select_build_machine(client, &targets, &machines, cancellation)
                    .await
                    .map_err(PreparationError::Selection)
            })
            .await?;
            let id = selected.machine.id;
            progress(Progress::Selected(Box::new(selected)));
            // Await terminal evidence and cleanup; cancelling this future would erase Unknown.
            builds.extend(
                build
                    .execute_remote_images(client, id, cancellation.clone(), |event| {
                        progress(Progress::Build(event))
                    })
                    .await?,
            );
        }
    }
    crate::build::bind(&mut intent, &builds)?;
    let machines = read(cancellation, async { Ok(client.machines().await?) }).await?;
    let plan = read(cancellation, async {
        Ok(crate::deploy::pipeline::plan_project(client, &intent, machines.clone()).await?)
    })
    .await?;
    if !builds.is_empty() {
        progress(Progress::Transfer);
    }
    let outcome = crate::deploy::pipeline::push_project_images(
        client,
        &builds,
        &machines,
        &plan,
        cancellation,
    )
    .await?;
    for image in outcome.pushed {
        progress(Progress::Delivered {
            image: image.image,
            machine_id: image.machine_id,
        });
    }
    if cancellation.is_cancelled() {
        return Err(PreparationError::Cancelled);
    }
    if !outcome.failures.is_empty() {
        return Err(PreparationError::Delivery(format!(
            "image push failed: {}",
            outcome.failures.join("; ")
        )));
    }
    Ok(Prepared { plan, builds })
}

async fn read<T>(
    cancel: &CancellationToken,
    work: impl std::future::Future<Output = Result<T, PreparationError>>,
) -> Result<T, PreparationError> {
    tokio::select! { biased; () = cancel.cancelled() => Err(PreparationError::Cancelled), result = work => result }
}
