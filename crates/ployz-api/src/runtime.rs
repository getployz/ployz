use ployz_types::model::{
    InstanceId, InstanceStatusRecord, MachineId, MachineMembership, RoutingEvent, RoutingState,
    ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeWatchFrame {
    Snapshot {
        state: RoutingState,
    },
    MachineUpsert {
        key: String,
        record: MachineMembership,
    },
    MachineRemove {
        key: String,
        id: MachineId,
    },
    RevisionUpsert {
        key: String,
        record: ServiceRevisionRecord,
    },
    RevisionRemove {
        key: String,
        namespace: Namespace,
        service: String,
        revision_hash: String,
    },
    ReleaseUpsert {
        key: String,
        record: ServiceReleaseRecord,
    },
    ReleaseRemove {
        key: String,
        namespace: Namespace,
        service: String,
    },
    InstanceUpsert {
        key: String,
        record: InstanceStatusRecord,
    },
    InstanceRemove {
        key: String,
        instance_id: InstanceId,
    },
    Error {
        code: String,
        message: String,
    },
    Heartbeat,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeStatePayload {
    pub state: RoutingState,
}

#[must_use]
pub fn machine_runtime_key(record: &MachineMembership) -> String {
    record.id.as_str().to_string()
}

#[must_use]
pub fn revision_runtime_key(record: &ServiceRevisionRecord) -> String {
    format!(
        "{}:{}:{}",
        record.namespace, record.service, record.revision_hash
    )
}

#[must_use]
pub fn release_runtime_key(record: &ServiceReleaseRecord) -> String {
    format!("{}:{}", record.namespace, record.service)
}

#[must_use]
pub fn instance_runtime_key(record: &InstanceStatusRecord) -> String {
    record.instance_id.as_str().to_string()
}

pub fn sort_routing_state(state: &mut RoutingState) {
    state.machines.sort_by_key(machine_runtime_key);
    state.revisions.sort_by_key(revision_runtime_key);
    state.releases.sort_by_key(release_runtime_key);
    state.instances.sort_by_key(instance_runtime_key);
}

#[must_use]
pub fn runtime_frame_from_event(event: RoutingEvent) -> RuntimeWatchFrame {
    match event {
        RoutingEvent::MachineUpsert(record) => RuntimeWatchFrame::MachineUpsert {
            key: machine_runtime_key(&record),
            record,
        },
        RoutingEvent::MachineRemoved { id } => RuntimeWatchFrame::MachineRemove {
            key: id.as_str().to_string(),
            id,
        },
        RoutingEvent::RevisionUpsert(record) => RuntimeWatchFrame::RevisionUpsert {
            key: revision_runtime_key(&record),
            record,
        },
        RoutingEvent::RevisionRemoved {
            namespace,
            service,
            revision_hash,
        } => RuntimeWatchFrame::RevisionRemove {
            key: format!("{namespace}:{service}:{revision_hash}"),
            namespace,
            service,
            revision_hash,
        },
        RoutingEvent::ReleaseUpsert(record) => RuntimeWatchFrame::ReleaseUpsert {
            key: release_runtime_key(&record),
            record,
        },
        RoutingEvent::ReleaseRemoved { namespace, service } => RuntimeWatchFrame::ReleaseRemove {
            key: format!("{namespace}:{service}"),
            namespace,
            service,
        },
        RoutingEvent::InstanceUpsert(record) => RuntimeWatchFrame::InstanceUpsert {
            key: instance_runtime_key(&record),
            record,
        },
        RoutingEvent::InstanceRemoved { instance_id } => RuntimeWatchFrame::InstanceRemove {
            key: instance_id.as_str().to_string(),
            instance_id,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeWatchFrame, instance_runtime_key, machine_runtime_key, release_runtime_key,
        revision_runtime_key, runtime_frame_from_event, sort_routing_state,
    };
    use crate::{
        ControlPlaneHealthState, ControlPlaneStatus, DaemonPayload, DaemonResponse,
        EdgeSyncHealthState, EdgeSyncStatus, MachineOperationInfo, MachineOperationPayload,
        MachineStoragePromotionFailure, MachineStoragePromotionFailureCause,
        MachineStoragePromotionPayload, MachineUpdatePayload, MachineUpdateRow, StatusPayload,
    };
    use ployz_types::model::{
        AuthorityNodePosture, DeployId, DrainState, InstanceId, InstancePhase,
        InstanceStatusRecord, MachineId, MachineLifecycle, MachineMembership, MachineTopology,
        NetworkLifecycle, OverlayIp, PublicKey, RoutingEvent, RoutingState, ServiceRelease,
        ServiceReleaseRecord, ServiceReleaseSlot, ServiceRevisionRecord, SlotId,
        StorageParticipation, StorageReplicaPolicy,
    };
    use ployz_types::spec::Namespace;
    use std::collections::BTreeMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn sort_routing_state_orders_all_collections_by_runtime_key() {
        let mut state = RoutingState {
            machines: vec![machine_record("machine-b"), machine_record("machine-a")],
            revisions: vec![
                revision_record("prod", "web", "bbbb"),
                revision_record("prod", "api", "aaaa"),
            ],
            releases: vec![release_record("prod", "web"), release_record("dev", "api")],
            instances: vec![
                instance_record("instance-b", "prod", "web"),
                instance_record("instance-a", "prod", "api"),
            ],
        };

        sort_routing_state(&mut state);

        assert_eq!(
            state
                .machines
                .iter()
                .map(machine_runtime_key)
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
        assert_eq!(
            state
                .revisions
                .iter()
                .map(revision_runtime_key)
                .collect::<Vec<_>>(),
            ["prod:api:aaaa", "prod:web:bbbb"]
        );
        assert_eq!(
            state
                .releases
                .iter()
                .map(release_runtime_key)
                .collect::<Vec<_>>(),
            ["dev:api", "prod:web"]
        );
        assert_eq!(
            state
                .instances
                .iter()
                .map(instance_runtime_key)
                .collect::<Vec<_>>(),
            ["instance-a", "instance-b"]
        );
    }

    #[test]
    fn instance_events_map_to_idempotent_watch_frames() {
        let mut record = instance_record("instance-1", "prod", "api");
        record.ready = true;

        let upsert = runtime_frame_from_event(RoutingEvent::InstanceUpsert(record.clone()));
        let removed = runtime_frame_from_event(RoutingEvent::InstanceRemoved {
            instance_id: record.instance_id.clone(),
        });

        assert_eq!(
            upsert,
            RuntimeWatchFrame::InstanceUpsert {
                key: String::from("instance-1"),
                record: record.clone(),
            }
        );
        assert_eq!(
            removed,
            RuntimeWatchFrame::InstanceRemove {
                key: String::from("instance-1"),
                instance_id: record.instance_id.clone(),
            }
        );
    }

    #[test]
    fn routing_events_for_all_collections_map_to_runtime_watch_frames() {
        let mut new_machine = machine_record("machine-1");
        new_machine.lifecycle = MachineLifecycle::Draining;
        let mut new_revision = revision_record("prod", "api", "rev-1");
        new_revision.spec_json = r#"{"image":"api:2"}"#.into();
        let mut new_release = release_record("prod", "api");
        new_release.release = ServiceRelease::direct(
            "rev-2",
            new_release.release.slots,
            new_release.release.updated_by_deploy_id,
            new_release.release.updated_at,
        );

        let cases = [
            (
                runtime_frame_from_event(RoutingEvent::MachineUpsert(new_machine.clone())),
                RuntimeWatchFrame::MachineUpsert {
                    key: String::from("machine-1"),
                    record: new_machine.clone(),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::MachineRemoved {
                    id: new_machine.id.clone(),
                }),
                RuntimeWatchFrame::MachineRemove {
                    key: String::from("machine-1"),
                    id: new_machine.id.clone(),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::RevisionUpsert(new_revision.clone())),
                RuntimeWatchFrame::RevisionUpsert {
                    key: String::from("prod:api:rev-1"),
                    record: new_revision.clone(),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::RevisionRemoved {
                    namespace: new_revision.namespace.clone(),
                    service: new_revision.service.clone(),
                    revision_hash: new_revision.revision_hash.clone(),
                }),
                RuntimeWatchFrame::RevisionRemove {
                    key: String::from("prod:api:rev-1"),
                    namespace: new_revision.namespace.clone(),
                    service: new_revision.service.clone(),
                    revision_hash: new_revision.revision_hash.clone(),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::ReleaseUpsert(new_release.clone())),
                RuntimeWatchFrame::ReleaseUpsert {
                    key: String::from("prod:api"),
                    record: new_release.clone(),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::ReleaseRemoved {
                    namespace: new_release.namespace.clone(),
                    service: new_release.service.clone(),
                }),
                RuntimeWatchFrame::ReleaseRemove {
                    key: String::from("prod:api"),
                    namespace: new_release.namespace.clone(),
                    service: new_release.service.clone(),
                },
            ),
        ];

        for (actual, expected) in cases {
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn runtime_frame_keys_are_deterministic() {
        assert_eq!(
            machine_runtime_key(&machine_record("machine-1")),
            "machine-1"
        );
        assert_eq!(
            revision_runtime_key(&revision_record("prod", "api", "abcd")),
            "prod:api:abcd"
        );
        assert_eq!(
            release_runtime_key(&release_record("prod", "api")),
            "prod:api"
        );
        assert_eq!(
            instance_runtime_key(&instance_record("instance-1", "prod", "api")),
            "instance-1"
        );
    }

    #[test]
    fn runtime_watch_frame_serialization_roundtrips() {
        let frame = RuntimeWatchFrame::InstanceUpsert {
            key: String::from("instance-1"),
            record: instance_record("instance-1", "prod", "api"),
        };

        let json = serde_json::to_value(&frame).expect("serialize runtime watch frame");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "instance_upsert",
                "key": "instance-1",
                "record": {
                    "instance_id": "instance-1",
                    "namespace": "prod",
                    "service": "api",
                    "slot_id": "slot-1",
                    "machine_id": "machine-1",
                    "revision_hash": "rev-1",
                    "deploy_id": "deploy-1",
                    "docker_container_id": "container-1",
                    "overlay_ip": "10.0.0.2",
                    "backend_ports": {
                        "http": 8080
                    },
                    "phase": "Ready",
                    "ready": true,
                    "drain_state": "None",
                    "error": null,
                    "started_at": 10,
                    "updated_at": 20
                }
            })
        );

        let decoded: RuntimeWatchFrame =
            serde_json::from_value(json).expect("deserialize runtime watch frame");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn runtime_watch_remove_frame_serialization_is_key_only() {
        let frame = RuntimeWatchFrame::ReleaseRemove {
            key: String::from("prod:api"),
            namespace: Namespace::new("prod"),
            service: String::from("api"),
        };

        let json = serde_json::to_value(&frame).expect("serialize runtime remove frame");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "release_remove",
                "key": "prod:api",
                "namespace": "prod",
                "service": "api"
            })
        );

        let decoded: RuntimeWatchFrame =
            serde_json::from_value(json).expect("deserialize runtime remove frame");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn runtime_watch_error_frame_serialization_roundtrips() {
        let frame = RuntimeWatchFrame::Error {
            code: String::from("RUNTIME_SUBSCRIPTION_FAILED"),
            message: String::from("routing event 'event-1' ack receiver closed"),
        };

        let json = serde_json::to_value(&frame).expect("serialize runtime error frame");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "error",
                "code": "RUNTIME_SUBSCRIPTION_FAILED",
                "message": "routing event 'event-1' ack receiver closed"
            })
        );

        let decoded: RuntimeWatchFrame =
            serde_json::from_value(json).expect("deserialize runtime error frame");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn daemon_error_response_preserves_structured_payload() {
        let response = DaemonResponse {
            ok: false,
            code: String::from("MACHINE_UPDATE_FAILED"),
            message: String::from("machine 'peer-1' update failed: refused"),
            payload: Some(DaemonPayload::MachineUpdate(MachineUpdatePayload {
                operation_id: String::from("update-1"),
                updated: Vec::new(),
                failed: vec![MachineUpdateRow {
                    id: String::from("peer-1"),
                    version: String::from("0.5.6"),
                    message: String::from("refused"),
                }],
            })),
        };

        let json = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(
            json,
            serde_json::json!({
                "ok": false,
                "code": "MACHINE_UPDATE_FAILED",
                "message": "machine 'peer-1' update failed: refused",
                "payload": {
                    "kind": "machine-update",
                    "operation_id": "update-1",
                    "failed": [{
                        "id": "peer-1",
                        "version": "0.5.6",
                        "message": "refused"
                    }]
                }
            })
        );
        let decoded: DaemonResponse = serde_json::from_value(json).expect("deserialize response");
        assert!(!decoded.ok);
        assert_eq!(decoded.code, "MACHINE_UPDATE_FAILED");
        let Some(DaemonPayload::MachineUpdate(payload)) = decoded.payload else {
            panic!("expected machine update payload");
        };
        assert_eq!(payload.operation_id, "update-1");
        let [failed] = payload.failed.as_slice() else {
            panic!("expected one failed row");
        };
        assert_eq!(failed.id, "peer-1");
        assert_eq!(failed.message, "refused");
    }

    #[test]
    fn daemon_storage_promotion_response_preserves_structured_payload() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("storage promotion complete"),
            payload: Some(DaemonPayload::MachineStoragePromotion(
                MachineStoragePromotionPayload {
                    operation_id: String::from("storage-promote-1"),
                    replicas: StorageReplicaPolicy::R3,
                    promoted: vec![String::from("m2"), String::from("m3")],
                    failed: vec![MachineStoragePromotionFailure {
                        machine_id: String::from("m4"),
                        cause: MachineStoragePromotionFailureCause::InvalidCandidate,
                        message: String::from("not active"),
                    }],
                },
            )),
        };

        let json = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(
            json,
            serde_json::json!({
                "ok": true,
                "code": "OK",
                "message": "storage promotion complete",
                "payload": {
                    "kind": "machine-storage-promotion",
                    "operation_id": "storage-promote-1",
                    "replicas": "r3",
                    "promoted": ["m2", "m3"],
                    "failed": [{
                        "machine_id": "m4",
                        "cause": "invalid_candidate",
                        "message": "not active"
                    }]
                }
            })
        );
        let decoded: DaemonResponse = serde_json::from_value(json).expect("deserialize response");
        let Some(DaemonPayload::MachineStoragePromotion(payload)) = decoded.payload else {
            panic!("expected machine storage promotion payload");
        };
        assert_eq!(payload.replicas, StorageReplicaPolicy::R3);
        assert_eq!(payload.promoted, vec!["m2", "m3"]);
    }

    #[test]
    fn daemon_operation_response_preserves_structured_failure_status() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("operation details"),
            payload: Some(DaemonPayload::MachineOperation(MachineOperationPayload {
                operation: MachineOperationInfo {
                    id: String::from("machine-add-1"),
                    kind: String::from("add"),
                    network_name: Some(String::from("alpha")),
                    targets: vec![String::from("host-a")],
                    status: String::from("interrupted"),
                    stage: String::from("bootstrap"),
                    started_at: 10,
                    updated_at: 20,
                    last_error: Some(String::from("daemon restarted before operation completed")),
                    machine_id: Some(MachineId::new("machine-a")),
                    invite_id: Some(String::from("invite-1")),
                    allocated_subnet: Some(String::from("10.210.1.0/24")),
                },
            })),
        };

        let json = serde_json::to_value(&response).expect("serialize operation response");

        assert_eq!(
            json,
            serde_json::json!({
                "ok": true,
                "code": "OK",
                "message": "operation details",
                "payload": {
                    "kind": "machine-operation",
                    "operation": {
                        "id": "machine-add-1",
                        "kind": "add",
                        "network_name": "alpha",
                        "targets": ["host-a"],
                        "status": "interrupted",
                        "stage": "bootstrap",
                        "started_at": 10,
                        "updated_at": 20,
                        "last_error": "daemon restarted before operation completed",
                        "machine_id": "machine-a",
                        "invite_id": "invite-1",
                        "allocated_subnet": "10.210.1.0/24"
                    }
                }
            })
        );

        let decoded: DaemonResponse =
            serde_json::from_value(json).expect("deserialize operation response");
        let Some(DaemonPayload::MachineOperation(payload)) = decoded.payload else {
            panic!("expected machine operation payload");
        };
        assert_eq!(payload.operation.status, "interrupted");
        assert_eq!(
            payload.operation.last_error.as_deref(),
            Some("daemon restarted before operation completed")
        );
    }

    #[test]
    fn daemon_status_response_preserves_edge_and_control_plane_uncertainty() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("status"),
            payload: Some(DaemonPayload::Status(StatusPayload {
                machine_id: String::from("founder"),
                public_key: PublicKey([1; 32]),
                version: String::from("0.5.5"),
                network: Some(String::from("alpha")),
                overlay_ip: Some(String::from("fd00::1")),
                network_lifecycle: Some(NetworkLifecycle::Running),
                local_machine_lifecycle: Some(MachineLifecycle::Active),
                local_authority: Some(AuthorityNodePosture::from_storage_participation(
                    true,
                    &StorageParticipation::default_authority(),
                )),
                mesh_phase: String::from("Running"),
                edge_sync: vec![EdgeSyncStatus {
                    service: String::from("gateway"),
                    stream: String::from("routing"),
                    state: EdgeSyncHealthState::Stale {
                        stale_since_unix_secs: 1_777_646_000,
                        failures_total: 7,
                    },
                }],
                nats_assets: Vec::new(),
                control_plane: vec![ControlPlaneStatus {
                    component: String::from("mesh_peer_sync"),
                    state: ControlPlaneHealthState::Unknown {
                        error: String::from("machine subscription closed"),
                    },
                }],
            })),
        };

        let json = serde_json::to_value(&response).expect("serialize status response");

        assert_eq!(
            json,
            serde_json::json!({
                "ok": true,
                "code": "OK",
                "message": "status",
                "payload": {
                    "kind": "status",
                    "machine_id": "founder",
                    "public_key": PublicKey([1; 32]),
                    "version": "0.5.5",
                    "network": "alpha",
                    "overlay_ip": "fd00::1",
                    "network_lifecycle": "Running",
                    "local_machine_lifecycle": "Active",
                    "local_authority": {
                        "kind": "authority_storage",
                        "authority_id": "auth-default"
                    },
                    "mesh_phase": "Running",
                    "edge_sync": [{
                        "service": "gateway",
                        "stream": "routing",
                        "state": "stale",
                        "stale_since_unix_secs": 1777646000u64,
                        "failures_total": 7u64
                    }],
                    "control_plane": [{
                        "component": "mesh_peer_sync",
                        "state": "unknown",
                        "error": "machine subscription closed"
                    }]
                }
            })
        );

        let decoded: DaemonResponse =
            serde_json::from_value(json).expect("deserialize status response");
        let Some(DaemonPayload::Status(payload)) = decoded.payload else {
            panic!("expected status payload");
        };
        let [edge] = payload.edge_sync.as_slice() else {
            panic!("expected one edge sync row");
        };
        match &edge.state {
            EdgeSyncHealthState::Stale {
                stale_since_unix_secs,
                failures_total,
            } => {
                assert_eq!(*stale_since_unix_secs, 1_777_646_000);
                assert_eq!(*failures_total, 7);
            }
            other => panic!("expected stale edge sync state, got {other:?}"),
        }
        let [control] = payload.control_plane.as_slice() else {
            panic!("expected one control-plane row");
        };
        match &control.state {
            ControlPlaneHealthState::Unknown { error } => {
                assert_eq!(error, "machine subscription closed");
            }
            other => panic!("expected unknown control-plane state, got {other:?}"),
        }
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            region_role: ployz_types::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: vec![String::from("127.0.0.1:51820")],
            lifecycle: MachineLifecycle::Active,
            storage_role: StorageParticipation::default_authority().into(),
            created_at: 1,
            updated_at: 2,
            labels: BTreeMap::new(),
        }
    }

    fn revision_record(
        namespace: &str,
        service: &str,
        revision_hash: &str,
    ) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: Namespace::new(namespace),
            service: service.into(),
            revision_hash: revision_hash.into(),
            spec_json: String::from("{}"),
            created_by: MachineId::new(String::from("machine-1")),
            created_at: 1,
        }
    }

    fn release_record(namespace: &str, service: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: Namespace::new(namespace),
            service: service.into(),
            release: ServiceRelease::direct(
                "rev-1",
                vec![ServiceReleaseSlot {
                    slot_id: SlotId::new(String::from("slot-1")),
                    machine_id: MachineId::new(String::from("machine-1")),
                    active_instance_id: InstanceId::new(String::from("instance-1")),
                    revision_hash: String::from("rev-1"),
                }],
                DeployId::new(String::from("deploy-1")),
                1,
            ),
        }
    }

    fn instance_record(id: &str, namespace: &str, service: &str) -> InstanceStatusRecord {
        let mut backend_ports = BTreeMap::new();
        backend_ports.insert(String::from("http"), 8080);
        InstanceStatusRecord {
            instance_id: InstanceId::new(id),
            namespace: Namespace::new(namespace),
            service: service.into(),
            slot_id: SlotId::new(String::from("slot-1")),
            machine_id: MachineId::new(String::from("machine-1")),
            revision_hash: String::from("rev-1"),
            deploy_id: DeployId::new(String::from("deploy-1")),
            docker_container_id: String::from("container-1"),
            overlay_ip: Some(Ipv4Addr::new(10, 0, 0, 2)),
            backend_ports,
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 10,
            updated_at: 20,
        }
    }
}
