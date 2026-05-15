use crate::daemon::DaemonState;
use ployz_api::{
    ControlPlaneHealthState, ControlPlaneStatus, NatsAssetHealthState, NatsAssetStatus,
};
use ployz_model::StorageReplicaPolicy;

impl DaemonState {
    pub(super) async fn control_plane_status(&self) -> Vec<ControlPlaneStatus> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        if self.runtime_is_memory_test() {
            return Vec::new();
        }
        let node_rpc_health_path = self
            .network_dir(&active.config.name.0)
            .join(crate::ipc::nats_listener::NATS_NODE_RPC_HEALTH_FILE);
        let cert_renewal_health_path = self
            .network_dir(&active.config.name.0)
            .join(crate::daemon::cert_renewal_health::NATS_CERT_RENEWAL_HEALTH_FILE);
        let network_dir = self.network_dir(&active.config.name.0);
        let mut status = Vec::new();
        match crate::ipc::nats_listener::load_health(node_rpc_health_path).await {
            Ok(health) => status.push(component_health_status("node_rpc_listener", &health)),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("node_rpc_listener"),
                state: ControlPlaneHealthState::Unknown {
                    error: format!("read listener health: {error}"),
                },
            }),
        }
        match crate::daemon::cert_renewal_health::load_health(cert_renewal_health_path).await {
            Ok(health) => status.push(component_health_status("cert_renewal_worker", &health)),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("cert_renewal_worker"),
                state: ControlPlaneHealthState::Unknown {
                    error: format!("read cert renewal health: {error}"),
                },
            }),
        }
        match crate::mesh_state::bootstrap::load_bootstrap_peer_seed_health(&network_dir) {
            Ok(Some(health)) => {
                status.push(component_health_status("bootstrap_peer_seed", &health))
            }
            Ok(None) => status.push(ControlPlaneStatus {
                component: String::from("bootstrap_peer_seed"),
                state: ControlPlaneHealthState::Unknown {
                    error: String::from("bootstrap peer seed health file missing"),
                },
            }),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("bootstrap_peer_seed"),
                state: ControlPlaneHealthState::Unknown {
                    error: format!("read bootstrap peer seed health: {error}"),
                },
            }),
        }
        for health in active.runtime.health_snapshot().components {
            status.push(named_component_health_status(health));
        }
        for health in active.mesh.task_health() {
            status.push(ControlPlaneStatus {
                component: health.name,
                state: match health.state {
                    ployz_orchestrator::mesh::tasks::MeshTaskHealthState::Healthy => {
                        ControlPlaneHealthState::Healthy
                    }
                    ployz_orchestrator::mesh::tasks::MeshTaskHealthState::Stale {
                        stale_since_unix_secs,
                        consecutive_failures,
                        last_error,
                    } => ControlPlaneHealthState::Stale {
                        stale_since_unix_secs,
                        consecutive_failures,
                        error: last_error,
                    },
                },
            });
        }
        status
    }
}

pub(super) fn storage_replica_intent_status(
    desired: StorageReplicaPolicy,
    assets: &[NatsAssetStatus],
) -> Option<ControlPlaneStatus> {
    if desired == StorageReplicaPolicy::Single {
        return None;
    }
    let expected = desired.replicas();
    let mut observed = 0usize;
    let mut stale_asset = None;
    for asset in assets {
        match &asset.state {
            NatsAssetHealthState::Healthy(replicas) => {
                observed += 1;
                if replicas.replicas != expected {
                    return Some(ControlPlaneStatus {
                        component: String::from("storage_replica_intent"),
                        state: ControlPlaneHealthState::Stale {
                            stale_since_unix_secs: ployz_time::now_unix_secs(),
                            consecutive_failures: 1,
                            error: format!(
                                "asset '{}' reports {} replicas; expected {}",
                                asset.name, replicas.replicas, expected
                            ),
                        },
                    });
                }
            }
            NatsAssetHealthState::Stale(replicas) => {
                observed += 1;
                if replicas.replicas != expected {
                    return Some(ControlPlaneStatus {
                        component: String::from("storage_replica_intent"),
                        state: ControlPlaneHealthState::Stale {
                            stale_since_unix_secs: ployz_time::now_unix_secs(),
                            consecutive_failures: 1,
                            error: format!(
                                "asset '{}' reports {} replicas; expected {}",
                                asset.name, replicas.replicas, expected
                            ),
                        },
                    });
                }
                stale_asset.get_or_insert_with(|| asset.name.clone());
            }
            NatsAssetHealthState::Unknown { error } => {
                return Some(ControlPlaneStatus {
                    component: String::from("storage_replica_intent"),
                    state: ControlPlaneHealthState::Unknown {
                        error: format!(
                            "asset '{}' replica observation unavailable: {error}",
                            asset.name
                        ),
                    },
                });
            }
        }
    }
    if let Some(asset_name) = stale_asset {
        return Some(ControlPlaneStatus {
            component: String::from("storage_replica_intent"),
            state: ControlPlaneHealthState::Stale {
                stale_since_unix_secs: ployz_time::now_unix_secs(),
                consecutive_failures: 1,
                error: format!("asset '{asset_name}' replica observation is stale"),
            },
        });
    }
    if observed == 0 {
        return Some(ControlPlaneStatus {
            component: String::from("storage_replica_intent"),
            state: ControlPlaneHealthState::Unknown {
                error: format!("no NATS assets observed for desired {expected} replicas"),
            },
        });
    }
    Some(ControlPlaneStatus {
        component: String::from("storage_replica_intent"),
        state: ControlPlaneHealthState::Healthy,
    })
}

pub(super) fn component_health_status(
    component: impl Into<String>,
    health: &ployz_supervision::ComponentHealth,
) -> ControlPlaneStatus {
    let state = match &health.state {
        ployz_supervision::ComponentHealthState::Healthy => ControlPlaneHealthState::Healthy,
        ployz_supervision::ComponentHealthState::Stale {
            stale_since_unix_secs,
            consecutive_failures,
            last_error,
        } => ControlPlaneHealthState::Stale {
            stale_since_unix_secs: *stale_since_unix_secs,
            consecutive_failures: *consecutive_failures,
            error: last_error.clone(),
        },
    };
    ControlPlaneStatus {
        component: component.into(),
        state,
    }
}

pub(super) fn named_component_health_status(
    health: ployz_supervision::NamedComponentHealth,
) -> ControlPlaneStatus {
    let component = health.name.clone();
    component_health_status(
        component,
        &ployz_supervision::ComponentHealth {
            updated_at_unix_secs: health.updated_at_unix_secs,
            state: health.state,
        },
    )
}
