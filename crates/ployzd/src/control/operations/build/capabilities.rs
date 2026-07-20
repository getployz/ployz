use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::build::{BuildTargetCapabilities, ClusterBuildTargetCapabilities};

use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{
    NatsMachineFactsReader, read_machine_placement_facts_with_gather_timeout,
};
use crate::control::role_client::machine_convergence::gather_dataplane_statuses;

use super::external_admission::read_external_build_pool_capabilities;
use super::placement::classify_cluster_build_capabilities;

const BUILD_TARGET_CAPABILITY_DEADLINE: Duration = Duration::from_secs(4);
const BUILD_TARGET_TESTIMONY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BuildTargetCapabilitiesReadError {
    #[error("{message}")]
    Unavailable { message: String },
}

pub(super) async fn read_build_target_capabilities(
    client: &async_nats::Client,
    facts: &NatsMachineFactsReader,
    intent_reader: &NatsIntentReader,
) -> Result<BuildTargetCapabilities, BuildTargetCapabilitiesReadError> {
    tokio::time::timeout(BUILD_TARGET_CAPABILITY_DEADLINE, async {
        let intent = intent_reader.intent().await.map_err(|error| {
            BuildTargetCapabilitiesReadError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let now_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| BuildTargetCapabilitiesReadError::Unavailable {
                message: error.to_string(),
            })?
            .as_secs();
        let short_facts = facts
            .clone()
            .with_request_timeout(BUILD_TARGET_TESTIMONY_TIMEOUT);
        let machine_lifecycles = intent
            .active_machines
            .iter()
            .map(|machine| (machine.machine_id.clone(), machine.lifecycle))
            .collect::<Vec<_>>();
        let projection = &intent.dataplane_projection;
        let (placement_facts, dataplane_statuses, external_pools) = tokio::join!(
            read_machine_placement_facts_with_gather_timeout(
                &short_facts,
                machine_lifecycles,
                BUILD_TARGET_TESTIMONY_TIMEOUT,
            ),
            gather_dataplane_statuses(
                &short_facts,
                projection
                    .declared_members()
                    .iter()
                    .map(|member| &member.machine_id),
            ),
            read_external_build_pool_capabilities(client, &intent, now_unix_seconds, None),
        );
        let external_pools = external_pools
            .map_err(|message| BuildTargetCapabilitiesReadError::Unavailable { message })?;
        Ok(BuildTargetCapabilities {
            cluster: ClusterBuildTargetCapabilities {
                machines: classify_cluster_build_capabilities(
                    &placement_facts,
                    projection,
                    &dataplane_statuses,
                ),
            },
            external_pools,
        })
    })
    .await
    .map_err(|_| BuildTargetCapabilitiesReadError::Unavailable {
        message: "capability gather exceeded its four-second deadline".to_owned(),
    })?
}
