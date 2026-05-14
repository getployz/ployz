use crate::daemon::DaemonState;
use async_nats::jetstream::stream::ClusterInfo;
use ployz_api::{NatsAssetHealthState, NatsAssetReplicaStatus, NatsAssetStatus};
use ployz_config::RuntimeTarget;
use ployz_nats::{NatsAssetScope, NatsAssetSpec, NatsScope, NatsStore};
use std::time::Duration;

const NATS_ASSET_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

impl DaemonState {
    pub(super) async fn nats_asset_status(&self) -> Vec<NatsAssetStatus> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let scope =
            NatsScope::local_for_storage_participation(&active.config.storage_participation);
        let manifest = NatsStore::asset_manifest_for_scope(&scope);
        match tokio::time::timeout(
            NATS_ASSET_PROBE_TIMEOUT,
            read_nats_asset_status(&client_url, &active.config),
        )
        .await
        {
            Ok(assets) => assets,
            Err(_) => nats_asset_probe_error(
                &scope,
                &manifest,
                format!("nats asset probe timed out for {client_url}"),
            ),
        }
    }
}

async fn read_nats_asset_status(
    client_url: &str,
    config: &crate::mesh_state::network::NetworkConfig,
) -> Vec<NatsAssetStatus> {
    let scope =
        ployz_nats::NatsScope::local_for_storage_participation(&config.storage_participation);
    let manifest = NatsStore::asset_manifest_for_scope(&scope);
    let store = match NatsStore::connect_with_scope(client_url, scope.clone()).await {
        Ok(store) => store,
        Err(error) => return nats_asset_probe_error(&scope, &manifest, error.to_string()),
    };
    let mut status = Vec::new();
    for asset in manifest {
        status.push(read_stream_status(&store, &asset).await);
    }
    status
}

async fn read_stream_status(store: &NatsStore, asset: &NatsAssetSpec) -> NatsAssetStatus {
    match store.asset_stream_info(asset).await {
        Ok(info) => nats_asset_status_from_info(store, asset, &info),
        Err(error) => nats_asset_status(
            store,
            asset,
            NatsAssetHealthState::Unknown {
                error: error.to_string(),
            },
        ),
    }
}

fn nats_asset_status_from_info(
    store: &NatsStore,
    asset: &NatsAssetSpec,
    info: &async_nats::jetstream::stream::Info,
) -> NatsAssetStatus {
    let replica_status = NatsAssetReplicaStatus {
        replicas: info.config.num_replicas,
        current_replicas: nats_current_replicas(info.config.num_replicas, info.cluster.as_ref()),
        offline_replicas: nats_offline_replicas(info.config.num_replicas, info.cluster.as_ref()),
        max_lag: nats_max_lag(info.cluster.as_ref()),
        leader: info
            .cluster
            .as_ref()
            .and_then(|cluster| cluster.leader.clone()),
    };
    let state = if nats_asset_is_healthy(&replica_status) {
        NatsAssetHealthState::Healthy(replica_status)
    } else {
        NatsAssetHealthState::Stale(replica_status)
    };
    nats_asset_status(store, asset, state)
}

fn nats_asset_status(
    store: &NatsStore,
    asset: &NatsAssetSpec,
    state: NatsAssetHealthState,
) -> NatsAssetStatus {
    nats_asset_status_for_scope(store.scope(), asset, state)
}

fn nats_asset_status_for_scope(
    scope: &NatsScope,
    asset: &NatsAssetSpec,
    state: NatsAssetHealthState,
) -> NatsAssetStatus {
    NatsAssetStatus {
        name: asset.name.clone(),
        kind: asset.kind.to_string(),
        data_bucket: asset.data_bucket,
        loss_impact: asset.loss_impact,
        installation: Some(scope.installation().to_string()),
        authority: nats_asset_authority(scope, asset.scope),
        domain: Some(nats_asset_domain(scope, asset.scope)),
        scope: Some(asset.scope.as_str().to_string()),
        state,
    }
}

fn nats_asset_authority(scope: &NatsScope, asset_scope: NatsAssetScope) -> Option<String> {
    match asset_scope {
        NatsAssetScope::AuthorityLocal => Some(scope.authority().to_string()),
        NatsAssetScope::InstallationRoot => None,
    }
}

fn nats_asset_domain(scope: &NatsScope, asset_scope: NatsAssetScope) -> String {
    match asset_scope {
        NatsAssetScope::AuthorityLocal => scope.authority_domain(),
        NatsAssetScope::InstallationRoot => scope.root_domain(),
    }
}

pub(super) fn nats_asset_is_healthy(status: &NatsAssetReplicaStatus) -> bool {
    status.current_replicas == status.replicas
        && status.offline_replicas == 0
        && status.max_lag == 0
}

pub(super) fn nats_current_replicas(replicas: usize, cluster: Option<&ClusterInfo>) -> usize {
    if replicas <= 1 && cluster.is_none() {
        return 1;
    }
    let Some(cluster) = cluster else {
        return 0;
    };
    usize::from(cluster.leader.is_some())
        + cluster
            .replicas
            .iter()
            .filter(|replica| replica.current && !replica.offline)
            .count()
}

pub(super) fn nats_offline_replicas(replicas: usize, cluster: Option<&ClusterInfo>) -> usize {
    if replicas <= 1 && cluster.is_none() {
        return 0;
    }
    let Some(cluster) = cluster else {
        return replicas;
    };
    let known_offline = cluster
        .replicas
        .iter()
        .filter(|replica| replica.offline)
        .count();
    let leader_missing = usize::from(cluster.leader.is_none());
    let known_replicas = 1 + cluster.replicas.len();
    known_offline + leader_missing + replicas.saturating_sub(known_replicas)
}

pub(super) fn nats_max_lag(cluster: Option<&ClusterInfo>) -> u64 {
    cluster
        .map(|cluster| {
            cluster
                .replicas
                .iter()
                .filter_map(|replica| replica.lag)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

pub(super) fn nats_asset_probe_error(
    scope: &NatsScope,
    manifest: &[NatsAssetSpec],
    error: String,
) -> Vec<NatsAssetStatus> {
    manifest
        .iter()
        .map(|asset| {
            nats_asset_status_for_scope(
                scope,
                asset,
                NatsAssetHealthState::Unknown {
                    error: error.clone(),
                },
            )
        })
        .collect()
}
