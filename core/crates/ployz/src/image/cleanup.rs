//! Image Cleanup: remove superseded build images from the Machines a Deploy delivered to.
//!
//! Only `repository:ployz-sha256-*` tags, which Direct Image Transfer creates, are
//! candidates. This client chooses them; each Machine removes only what it is told
//! and refuses anything a Container uses.

use std::{collections::BTreeMap, time::Duration};

use ployz_core::{
    DescribeContractRequest, DiskSpace, ImageCleanupReport, ImageSummary, ListImagesRequest,
    MachineCleanupResult, MachineId, MachineImageCleanup, MachineTarget, PruneTarget,
    REMOVE_IMAGES_CAPABILITY, RemoveImagesRequest, RpcError, op,
};

use crate::{build::BuiltService, connect::Client, deploy::DeployPlan};

const RETENTION_TAG: &str = ":ployz-sha256-";
/// Unused images kept for rollback.
const KEEP: usize = 3;
/// Unused images kept when the Docker root is nearly full.
const KEEP_UNDER_PRESSURE: usize = 1;
/// Beyond [`KEEP`], an unused image goes once this idle.
const IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;
/// Below this free share of the Docker root, keep only [`KEEP_UNDER_PRESSURE`].
const PRESSURE_FREE_PERCENT: u64 = 20;
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
    let mut repositories = BTreeMap::<MachineId, Vec<&str>>::new();
    for target in targets {
        repositories
            .entry(target.machine_id)
            .or_default()
            .push(&target.repository);
    }
    let machines = futures_util::future::join_all(repositories.into_iter().map(
        |(machine_id, repositories)| async move {
            let result = match tokio::time::timeout(
                MACHINE_TIMEOUT,
                clean(client, machine_id, &repositories),
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
    repositories: &[&str],
) -> Result<MachineCleanupResult, RpcError> {
    let target = MachineTarget::from(&machine_id);
    let contract = client
        .invoke::<op::DescribeContract>(DescribeContractRequest {}, &target, None)
        .await?;
    if !contract.supports(REMOVE_IMAGES_CAPABILITY) {
        return Ok(MachineCleanupResult::Unsupported);
    }
    let now = chrono::Utc::now().timestamp();
    let mut references = Vec::new();
    for &repository in repositories {
        let store = client
            .invoke::<op::ListImages>(
                ListImagesRequest {
                    reference: Some(format!("{repository}{RETENTION_TAG}*")),
                    last_tagged: true,
                },
                &target,
                None,
            )
            .await?;
        references.extend(superseded(
            &store.images,
            repository,
            under_pressure(store.docker_root),
            now,
        ));
    }
    if references.is_empty() {
        return Ok(MachineCleanupResult::Cleaned {
            removals: Vec::new(),
        });
    }
    let removed = client
        .invoke::<op::RemoveImages>(RemoveImagesRequest { references }, &target, None)
        .await?;
    Ok(MachineCleanupResult::Cleaned {
        removals: removed.results,
    })
}

/// Retention tags in `repository` to remove, newest kept. Images a Container uses,
/// or whose use Docker did not count, are neither kept nor removed.
fn superseded(images: &[ImageSummary], repository: &str, pressure: bool, now: i64) -> Vec<String> {
    let prefix = format!("{repository}{RETENTION_TAG}");
    let mut unused = images
        .iter()
        .filter(|image| image.containers == 0)
        .flat_map(|image| {
            image
                .repo_tags
                .iter()
                .filter(|tag| tag.starts_with(&prefix))
                .map(|tag| (image.last_tagged, tag))
        })
        .collect::<Vec<_>>();
    // Newest first; an unknown tag time sorts oldest.
    unused.sort_by_key(|(tagged, _)| std::cmp::Reverse(*tagged));
    let keep = if pressure { KEEP_UNDER_PRESSURE } else { KEEP };
    unused
        .into_iter()
        .skip(keep)
        .filter(|(tagged, _)| pressure || tagged.is_some_and(|at| now - at > IDLE_SECONDS))
        .map(|(_, tag)| tag.clone())
        .collect()
}

fn under_pressure(space: Option<DiskSpace>) -> bool {
    space.is_some_and(|space| {
        u128::from(space.free_bytes) * 100
            < u128::from(space.total_bytes) * u128::from(PRESSURE_FREE_PERCENT)
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

    const NOW: i64 = 100 * 24 * 60 * 60;
    const DAY: i64 = 24 * 60 * 60;

    fn image(tag: &str, days_ago: Option<i64>, containers: i64) -> ImageSummary {
        ImageSummary {
            id: format!("sha256:{tag}"),
            repo_tags: vec![format!("ployz-build/web:ployz-sha256-{tag}")],
            created: 0,
            size: 1,
            containers,
            platforms: Vec::new(),
            last_tagged: days_ago.map(|days| NOW - days * DAY),
        }
    }

    fn removed(images: &[ImageSummary], pressure: bool) -> Vec<String> {
        superseded(images, "ployz-build/web", pressure, NOW)
            .into_iter()
            .map(|tag| {
                tag.trim_start_matches("ployz-build/web:ployz-sha256-")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn keeps_three_unused_and_removes_older_idle_ones() {
        let images = [
            image("running", Some(30), 1),
            image("uncounted", Some(30), -1),
            image("a", Some(1), 0),
            image("b", Some(2), 0),
            image("c", Some(3), 0),
            image("recent", Some(4), 0),
            image("old", Some(8), 0),
            image("unknown", None, 0),
        ];
        assert_eq!(removed(&images, false), ["old"]);
    }

    #[test]
    fn pressure_keeps_only_the_newest_unused() {
        let images = [
            image("running", Some(30), 1),
            image("a", Some(1), 0),
            image("b", Some(2), 0),
            image("unknown", None, 0),
        ];
        assert_eq!(removed(&images, true), ["b", "unknown"]);
    }

    #[test]
    fn ignores_other_repositories_and_tags() {
        let mut other = image("x", Some(90), 0);
        other.repo_tags = vec![
            "ployz-build/web-api:ployz-sha256-x".into(),
            "ployz-build/web:latest".into(),
        ];
        assert!(removed(&[other.clone(), other.clone(), other.clone(), other], true).is_empty());
    }

    #[test]
    fn pressure_starts_below_a_fifth_free() {
        let space = |free_bytes| {
            Some(DiskSpace {
                total_bytes: 100,
                free_bytes,
            })
        };
        assert!(under_pressure(space(19)));
        assert!(!under_pressure(space(20)));
        assert!(!under_pressure(None));
    }

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
