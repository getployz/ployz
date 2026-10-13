//! Image Cleanup: remove superseded build images from the Machines a Deploy delivered to.
//!
//! This client names the repositories each Machine received; each Machine chooses
//! which of their build images are superseded and keeps anything a Container uses.

use std::{collections::BTreeMap, time::Duration};

use ployz_core::{
    DescribeContractRequest, ImageCleanupReport, MachineCleanupResult, MachineId,
    MachineImageCleanup, MachineTarget, PRUNE_IMAGES_CAPABILITY, PruneImagesRequest, PruneTarget,
    RpcError, op,
};

use crate::{build::BuiltService, connect::Client, deploy::DeployPlan};

const MACHINE_TIMEOUT: Duration = Duration::from_secs(60);

/// Each built repository paired with each Machine the plan runs its Service on, and
/// with the Machine holding the build, which a Build Grant push may have tagged.
#[must_use]
pub(crate) fn prune_targets(plan: &DeployPlan, builds: &[BuiltService]) -> Vec<PruneTarget> {
    let mut targets = builds
        .iter()
        .filter_map(|service| {
            let repository = short_repository(&service.image)?;
            Some(
                plan.operations
                    .iter()
                    .filter(|row| {
                        row.operation
                            .spec()
                            .is_some_and(|spec| spec.name == service.name)
                    })
                    .map(|row| row.machine_id)
                    .chain([service.machine_id])
                    .map(move |machine_id| PruneTarget {
                        machine_id,
                        repository: repository.clone(),
                    }),
            )
        })
        .flatten()
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    targets
}

/// Clean every target Machine in parallel. Never fails: each Machine reports its own
/// result, and a Machine that does not answer within a minute is `unknown`.
pub async fn prune_images(client: &Client, targets: &[PruneTarget]) -> ImageCleanupReport {
    let mut repositories = BTreeMap::<MachineId, Vec<String>>::new();
    for target in targets {
        repositories
            .entry(target.machine_id)
            .or_default()
            .push(target.repository.clone());
    }
    let machines = futures_util::future::join_all(repositories.into_iter().map(
        |(machine_id, repositories)| async move {
            let result = match tokio::time::timeout(
                MACHINE_TIMEOUT,
                clean(client, machine_id, repositories),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => MachineCleanupResult::Unknown {
                    message: error.to_string(),
                },
                Err(_) => MachineCleanupResult::Unknown {
                    message: format!("no answer within {}s", MACHINE_TIMEOUT.as_secs()),
                },
            };
            MachineImageCleanup { machine_id, result }
        },
    ))
    .await;
    ImageCleanupReport { machines }
}

async fn clean(
    client: &Client,
    machine_id: MachineId,
    repositories: Vec<String>,
) -> Result<MachineCleanupResult, RpcError> {
    let target = MachineTarget::from(&machine_id);
    let contract = client
        .invoke::<op::DescribeContract>(DescribeContractRequest {}, &target, None)
        .await?;
    if !contract.supports(PRUNE_IMAGES_CAPABILITY) {
        return Ok(MachineCleanupResult::Unsupported);
    }
    let removed = client
        .invoke::<op::PruneImages>(PruneImagesRequest { repositories }, &target, None)
        .await?;
    Ok(MachineCleanupResult::Cleaned {
        removals: removed.results,
    })
}

/// Docker's short name, the form `docker image ls` prints and filters on.
fn short_repository(image: &str) -> Option<String> {
    let reference = image.parse::<oci_client::Reference>().ok()?;
    let repository = reference.repository();
    Some(match reference.registry() {
        "docker.io" => repository
            .strip_prefix("library/")
            .unwrap_or(repository)
            .to_owned(),
        registry => format!("{registry}/{repository}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repositories_use_docker_short_names() {
        assert_eq!(
            short_repository("ployz-build/web:pending").as_deref(),
            Some("ployz-build/web")
        );
        assert_eq!(short_repository("nginx").as_deref(), Some("nginx"));
        assert_eq!(
            short_repository("ghcr.io/acme/api:1").as_deref(),
            Some("ghcr.io/acme/api")
        );
    }
}
