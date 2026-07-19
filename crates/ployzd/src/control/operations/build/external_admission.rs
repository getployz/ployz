use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{MAX_CONCURRENT_MACHINE_READS, call_machine};
use futures_util::{StreamExt, stream};
use ployz_core::build::{
    BuildAdapter, BuildExecutorIdentity, BuildExecutorReadiness, BuildExecutorReadinessAnswer,
    BuildExecutorReadinessRequest, BuildExecutorReadinessTestimony,
    BuildPlatformExecutorAssignment, BuildPlatforms, BuildPoolId, ExternalBuildExecutorCandidate,
    ExternalBuildPlacementError, place_external_build_platforms,
    reconcile_build_executor_readiness_at,
};
use ployz_core::image::{ImageBlobCheckOk, ImageBlobCheckRequest, ImageRpcDomainError};
use ployz_core::machine::MachineLifecycle;
use ployz_core::nats_config::{CredentialGrant, NatsAuthorizationGrant};
use ployz_nats::service_runtime::request_json;
use ployz_nats::subjects::{
    BuildExecutorServiceEndpoint, MachineServiceEndpoint, build_executor_service,
};

const EXTERNAL_BUILD_ADMISSION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExternalBuildAdmissionError {
    NoCapableExecutor {
        pool_id: BuildPoolId,
        platform: ployz_core::image::OciPlatform,
    },
    NoReachableImageSeed {
        pool_id: BuildPoolId,
    },
    Unavailable {
        message: String,
    },
}

pub(super) async fn preflight_external_build(
    client: &async_nats::Client,
    intent_reader: &NatsIntentReader,
    pool_id: &BuildPoolId,
    platforms: &BuildPlatforms,
    adapter: &BuildAdapter,
) -> Result<Vec<BuildPlatformExecutorAssignment>, ExternalBuildAdmissionError> {
    let intent =
        intent_reader
            .intent()
            .await
            .map_err(|error| ExternalBuildAdmissionError::Unavailable {
                message: error.to_string(),
            })?;
    let now_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ExternalBuildAdmissionError::Unavailable {
            message: error.to_string(),
        })?
        .as_secs();
    let credentials = intent
        .nats_authorizations
        .iter()
        .filter_map(|authorization| match authorization {
            NatsAuthorizationGrant::Credential(credential)
                if credential
                    .role
                    .build_executor_identity()
                    .is_some_and(|identity| identity.pool_id == *pool_id) =>
            {
                Some(credential.clone())
            }
            NatsAuthorizationGrant::Credential(_) | NatsAuthorizationGrant::Internal { .. } => None,
        })
        .collect::<Vec<CredentialGrant>>();
    let identities = credentials
        .iter()
        .filter(|credential| credential.role.is_active_at(now_unix_seconds))
        .filter_map(|credential| credential.role.build_executor_identity())
        .collect::<Vec<_>>();
    let readiness_answers = gather_readiness(client, identities).await;
    let testimony =
        reconcile_build_executor_readiness_at(&credentials, readiness_answers, now_unix_seconds)
            .map_err(|error| ExternalBuildAdmissionError::Unavailable {
                message: error.to_string(),
            })?;
    let candidates = candidates_from_testimony(pool_id, platforms, adapter, testimony);
    let active_machines = intent
        .active_machines
        .into_iter()
        .filter(|machine| machine.lifecycle == MachineLifecycle::Active)
        .map(|machine| machine.machine_id)
        .collect::<Vec<_>>();
    let reachable_image_seeds = gather_reachable_image_seeds(client, active_machines).await;
    place_external_build_platforms(pool_id, platforms, &candidates, &reachable_image_seeds)
        .map_err(ExternalBuildAdmissionError::from)
}

async fn gather_readiness(
    client: &async_nats::Client,
    identities: Vec<BuildExecutorIdentity>,
) -> Vec<BuildExecutorReadinessAnswer<BuildExecutorReadiness>> {
    let deadline = tokio::time::Instant::now() + EXTERNAL_BUILD_ADMISSION_TIMEOUT;
    stream::iter(identities.into_iter().map(|identity| async move {
        let subject = build_executor_service(
            &identity.pool_id,
            &identity.executor_id,
            BuildExecutorServiceEndpoint::ReadinessGet,
        );
        tokio::time::timeout_at(
            deadline,
            request_json::<_, BuildExecutorReadinessAnswer<BuildExecutorReadiness>>(
                client,
                subject,
                &BuildExecutorReadinessRequest {},
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            ),
        )
        .await
        .ok()
        .and_then(Result::ok)
    }))
    .buffer_unordered(MAX_CONCURRENT_MACHINE_READS)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .flatten()
    .collect()
}

fn candidates_from_testimony(
    pool_id: &BuildPoolId,
    platforms: &BuildPlatforms,
    adapter: &BuildAdapter,
    testimony: Vec<BuildExecutorReadinessTestimony<BuildExecutorReadiness>>,
) -> Vec<ExternalBuildExecutorCandidate> {
    testimony
        .into_iter()
        .filter_map(|testimony| match testimony {
            BuildExecutorReadinessTestimony::Answered {
                identity,
                readiness:
                    BuildExecutorReadiness {
                        native_platform,
                        capability,
                    },
            } if identity.pool_id == *pool_id
                && platforms.contains(&native_platform)
                && capability.supports(adapter) =>
            {
                Some(ExternalBuildExecutorCandidate {
                    pool_id: identity.pool_id,
                    executor_id: identity.executor_id,
                    platform: native_platform,
                })
            }
            BuildExecutorReadinessTestimony::Answered { .. }
            | BuildExecutorReadinessTestimony::Silent { .. } => None,
        })
        .collect()
}

async fn gather_reachable_image_seeds(
    client: &async_nats::Client,
    machine_ids: Vec<ployz_core::ids::MachineId>,
) -> BTreeSet<ployz_core::ids::MachineId> {
    let deadline = tokio::time::Instant::now() + EXTERNAL_BUILD_ADMISSION_TIMEOUT;
    stream::iter(machine_ids.into_iter().map(|machine_id| async move {
        tokio::time::timeout_at(
            deadline,
            call_machine::<ImageBlobCheckOk, ImageRpcDomainError>(
                client,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                &machine_id,
                MachineServiceEndpoint::ImageBlobCheck,
                &ImageBlobCheckRequest {
                    digests: Vec::new(),
                },
            ),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|answer| answer.present.is_empty().then_some(machine_id))
    }))
    .buffer_unordered(MAX_CONCURRENT_MACHINE_READS)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .flatten()
    .collect()
}

impl From<ExternalBuildPlacementError> for ExternalBuildAdmissionError {
    fn from(error: ExternalBuildPlacementError) -> Self {
        match error {
            ExternalBuildPlacementError::NoCapableExecutor { pool_id, platform } => {
                Self::NoCapableExecutor { pool_id, platform }
            }
            ExternalBuildPlacementError::NoReachableImageSeed { pool_id } => {
                Self::NoReachableImageSeed { pool_id }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{BuildAdapter, BuildExecutorCapability};
    use ployz_core::ids::BuildExecutorId;
    use ployz_core::image::OciPlatform;

    fn platform() -> OciPlatform {
        OciPlatform::try_new("linux", "amd64").expect("platform")
    }

    #[test]
    fn candidates_require_requested_pool_platform_and_adapter_capability() {
        let pool_id = BuildPoolId::try_new("pool-a").expect("pool");
        let platforms = BuildPlatforms::try_new([platform()]).expect("platforms");
        let identity = |pool: &str, executor: &str| BuildExecutorIdentity {
            pool_id: BuildPoolId::try_new(pool).expect("pool"),
            executor_id: BuildExecutorId::try_new(executor).expect("executor"),
        };
        let answered = |identity, capability| BuildExecutorReadinessTestimony::Answered {
            identity,
            readiness: BuildExecutorReadiness {
                native_platform: platform(),
                capability,
            },
        };
        let adapter = BuildAdapter::Railpack {
            cache_scope: ployz_core::build::BuildCacheScope::try_new("scope").expect("scope"),
        };
        let candidates = candidates_from_testimony(
            &pool_id,
            &platforms,
            &adapter,
            vec![
                answered(
                    identity("pool-b", "other"),
                    BuildExecutorCapability::DockerfileAndRailpack,
                ),
                answered(
                    identity("pool-a", "dockerfile"),
                    BuildExecutorCapability::DockerfileOnly,
                ),
                answered(
                    identity("pool-a", "ready"),
                    BuildExecutorCapability::DockerfileAndRailpack,
                ),
                BuildExecutorReadinessTestimony::Silent {
                    identity: identity("pool-a", "silent"),
                },
            ],
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates
                .first()
                .map(|candidate| candidate.executor_id.as_str()),
            Some("ready")
        );
    }
}
