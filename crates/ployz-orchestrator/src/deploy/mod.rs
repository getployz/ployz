use crate::error::{Error, Result};
use crate::model::{
    DeployApplyResult, DeployChangeKind, DeployEvent, DeployId, DeployPreview, DeployRecord,
    DeployState, InstanceId, MachineId, ServicePlan, ServiceRelease, ServiceReleaseRecord,
    ServiceReleaseSlot, ServiceRevisionRecord, ServiceRoutingPolicy, SlotId, SlotPlan,
};
use ployz_runtime_api::{DeploySession, DeploySessionFactory, StartCandidateRequest};
use ployz_store_api::{
    DeployCommit, DeployCommitStore, DeployReadStore, DeployWriteStore, MachineStore,
};
use ployz_types::spec::{DeployManifest, Placement, ServiceSpec, stable_hash_hex};
use ployz_types::time::now_unix_secs;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;

use crate::machine_liveness::machine_is_fresh;

#[derive(Debug, Clone)]
struct DesiredSlot {
    slot_id: SlotId,
    machine_id: MachineId,
}

pub async fn preview(
    deploy_read: &dyn DeployReadStore,
    machine_store: &dyn MachineStore,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployPreview> {
    manifest
        .validate()
        .map_err(|error| Error::operation("deploy_preview", error))?;
    let namespace = &manifest.namespace;

    let current_releases = deploy_read.list_service_releases(namespace).await?;
    let machines = machine_store.list_machines().await?;
    let desired_machines = deployable_machines(&machines, local_machine_id, now_unix_secs());
    let current_release_map: HashMap<String, ServiceReleaseRecord> = current_releases
        .into_iter()
        .map(|record| (record.service.clone(), record))
        .collect();
    let current_slots_by_service = current_slots_by_service(&current_release_map);

    let manifest_hash = stable_hash_hex(
        serde_json::to_vec(manifest)
            .map_err(|error| {
                Error::operation("deploy_preview", format!("serialize manifest: {error}"))
            })?
            .as_slice(),
    );

    let mut participants = BTreeSet::new();
    for machine_id in &desired_machines {
        participants.insert(machine_id.clone());
    }

    let mut services = Vec::new();
    for spec in &manifest.services {
        let revision_hash = spec
            .revision_hash()
            .map_err(|error| Error::operation("deploy_preview", error))?;
        let desired_slots = desired_slots(
            spec,
            &desired_machines,
            current_slots_by_service.get(&spec.name).map(Vec::as_slice),
        )?;
        let current_service_slots = current_slots_by_service
            .get(&spec.name)
            .cloned()
            .unwrap_or_default();
        let current_release = current_release_map.get(&spec.name);

        let mut slot_plans = Vec::new();
        for desired_slot in desired_slots {
            participants.insert(desired_slot.machine_id.clone());
            let current_slot = current_service_slots
                .iter()
                .find(|slot| slot.slot_id == desired_slot.slot_id);
            let action = match current_slot {
                Some(slot)
                    if slot.machine_id == desired_slot.machine_id
                        && slot.revision_hash == revision_hash =>
                {
                    DeployChangeKind::Unchanged
                }
                Some(_) => DeployChangeKind::Replace,
                None => DeployChangeKind::Create,
            };
            slot_plans.push(SlotPlan {
                slot_id: desired_slot.slot_id,
                machine_id: desired_slot.machine_id,
                current_instance_id: current_slot.map(|slot| slot.active_instance_id.clone()),
                next_instance_id: None,
                current_revision_hash: current_slot.map(|slot| slot.revision_hash.clone()),
                next_revision_hash: Some(revision_hash.clone()),
                action,
            });
        }

        for slot in &current_service_slots {
            participants.insert(slot.machine_id.clone());
        }

        let action = if slot_plans
            .iter()
            .all(|plan| plan.action == DeployChangeKind::Unchanged)
            && current_release.map(|release| release.release.primary_revision_hash.as_str())
                == Some(revision_hash.as_str())
        {
            DeployChangeKind::Unchanged
        } else if current_release.is_none() {
            DeployChangeKind::Create
        } else {
            DeployChangeKind::Replace
        };

        services.push(ServicePlan {
            service: spec.name.clone(),
            current_revision_hash: current_release
                .map(|release| release.release.primary_revision_hash.clone()),
            next_revision_hash: Some(revision_hash),
            slots: slot_plans,
            action,
        });
    }

    for (service, slots) in current_slots_by_service {
        if manifest.services.iter().any(|spec| spec.name == service) {
            continue;
        }
        for slot in &slots {
            participants.insert(slot.machine_id.clone());
        }
        services.push(ServicePlan {
            service: service.clone(),
            current_revision_hash: current_release_map
                .get(&service)
                .map(|release| release.release.primary_revision_hash.clone()),
            next_revision_hash: None,
            slots: slots
                .into_iter()
                .map(|slot| SlotPlan {
                    slot_id: slot.slot_id,
                    machine_id: slot.machine_id,
                    current_instance_id: Some(slot.active_instance_id),
                    next_instance_id: None,
                    current_revision_hash: Some(slot.revision_hash),
                    next_revision_hash: None,
                    action: DeployChangeKind::Remove,
                })
                .collect(),
            action: DeployChangeKind::Remove,
        });
    }

    Ok(DeployPreview {
        namespace: namespace.clone(),
        manifest_hash,
        participants: participants.into_iter().collect(),
        services,
        warnings: Vec::new(),
    })
}

pub async fn apply(
    deploy_read: &dyn DeployReadStore,
    deploy_write: &dyn DeployWriteStore,
    deploy_commit: &dyn DeployCommitStore,
    machine_store: &dyn MachineStore,
    session_factory: &dyn DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployApplyResult> {
    let namespace = &manifest.namespace;
    let deploy_id = DeployId(Uuid::new_v4().to_string());
    let started_at = now_unix_secs();
    let initial_preview = preview(deploy_read, machine_store, local_machine_id, manifest).await?;
    let machines = machine_store.list_machines().await?;
    let machine_map: HashMap<MachineId, crate::model::MachineRecord> = machines
        .iter()
        .map(|machine| (machine.id.clone(), machine.clone()))
        .collect();

    let mut events = vec![];
    let mut sorted_participants = initial_preview.participants.clone();
    sorted_participants.sort();

    let mut sessions: BTreeMap<MachineId, Box<dyn DeploySession>> = BTreeMap::new();
    for participant in &sorted_participants {
        let Some(machine) = machine_map.get(participant) else {
            return Err(Error::operation(
                "deploy_apply",
                format!(
                    "participant '{}' is missing from machine inventory",
                    participant
                ),
            ));
        };
        let (session, instances) = session_factory
            .open(machine, namespace, &deploy_id, local_machine_id)
            .await?;
        events.push(DeployEvent {
            step: "lock".into(),
            message: format!(
                "acquired lock on '{}' ({} instances)",
                participant,
                instances.len()
            ),
        });
        sessions.insert(participant.clone(), session);
    }

    let result = async {
        let final_preview = preview(deploy_read, machine_store, local_machine_id, manifest).await?;
        if final_preview.participants != initial_preview.participants {
            return Err(Error::operation(
                "deploy_apply",
                "participant set changed after lock acquisition; retry deploy",
            ));
        }

        let mut deploy_record = DeployRecord {
            deploy_id: deploy_id.clone(),
            namespace: namespace.clone(),
            coordinator_machine_id: local_machine_id.clone(),
            manifest_hash: final_preview.manifest_hash.clone(),
            state: DeployState::Applying,
            started_at,
            committed_at: None,
            finished_at: None,
            summary_json: serde_json::to_string(&final_preview).map_err(|error| {
                Error::operation("deploy_apply", format!("serialize preview: {error}"))
            })?,
        };
        deploy_write.upsert_deploy(&deploy_record).await?;

        let current_slots_by_service = current_slots_by_service_from_releases(
            &deploy_read.list_service_releases(namespace).await?,
        );
        let desired_machines = deployable_machines(&machines, local_machine_id, now_unix_secs());
        let mut removed_services = Vec::new();
        let mut committed_releases = Vec::new();
        let mut committed_slots = Vec::new();

        for spec in &manifest.services {
            let revision_hash = spec
                .revision_hash()
                .map_err(|error| Error::operation("deploy_apply", error))?;
            let spec_json = spec
                .canonical_revision_json()
                .map_err(|error| Error::operation("deploy_apply", error))?;
            deploy_write
                .upsert_service_revision(&ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: spec.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: spec_json.clone(),
                    created_by: local_machine_id.clone(),
                    created_at: started_at,
                })
                .await?;

            let desired = desired_slots(
                spec,
                &desired_machines,
                current_slots_by_service.get(&spec.name).map(Vec::as_slice),
            )?;

            let mut next_slots = Vec::new();
            for desired_slot in desired {
                let current_slot = current_slots_by_service.get(&spec.name).and_then(|slots| {
                    slots
                        .iter()
                        .find(|slot| slot.slot_id == desired_slot.slot_id)
                });
                let keep_current = current_slot.is_some_and(|slot| {
                    slot.machine_id == desired_slot.machine_id
                        && slot.revision_hash == revision_hash
                });

                let active_instance_id = if keep_current {
                    let Some(slot) = current_slot else {
                        return Err(Error::operation("deploy_apply", "missing current slot"));
                    };
                    slot.active_instance_id.clone()
                } else {
                    let instance_id = InstanceId(Uuid::new_v4().to_string());
                    events.push(DeployEvent {
                        step: "start_candidate".into(),
                        message: format!(
                            "starting {} slot {} as instance {} on {}",
                            spec.name, desired_slot.slot_id, instance_id, desired_slot.machine_id
                        ),
                    });
                    let Some(session) = sessions.get_mut(&desired_slot.machine_id) else {
                        return Err(Error::operation(
                            "deploy_apply",
                            format!(
                                "no session was available for machine '{}'",
                                desired_slot.machine_id
                            ),
                        ));
                    };
                    let status = session
                        .start_candidate(StartCandidateRequest {
                            service: spec.name.clone(),
                            slot_id: desired_slot.slot_id.clone(),
                            instance_id,
                            spec_json: spec_json.clone(),
                        })
                        .await?;
                    deploy_write.upsert_instance_status(&status).await?;
                    status.instance_id
                };

                next_slots.push(ServiceReleaseSlot {
                    slot_id: desired_slot.slot_id,
                    machine_id: desired_slot.machine_id,
                    active_instance_id,
                    revision_hash: revision_hash.clone(),
                });
            }

            committed_releases.push(ServiceReleaseRecord {
                namespace: namespace.clone(),
                service: spec.name.clone(),
                release: ServiceRelease {
                    primary_revision_hash: revision_hash.clone(),
                    referenced_revision_hashes: vec![revision_hash.clone()],
                    routing: ServiceRoutingPolicy::Direct {
                        revision_hash: revision_hash.clone(),
                    },
                    slots: next_slots.clone(),
                    updated_by_deploy_id: deploy_id.clone(),
                    updated_at: now_unix_secs(),
                },
            });
            committed_slots.extend(next_slots);
        }

        for service in final_preview
            .services
            .iter()
            .filter(|plan| plan.action == DeployChangeKind::Remove)
            .map(|plan| plan.service.clone())
        {
            removed_services.push(service);
        }

        deploy_record.state = DeployState::Committed;
        deploy_record.committed_at = Some(now_unix_secs());
        deploy_record.finished_at = deploy_record.committed_at;
        deploy_record.summary_json = serde_json::to_string(&final_preview).map_err(|error| {
            Error::operation("deploy_apply", format!("serialize preview: {error}"))
        })?;

        deploy_commit
            .apply_deploy_commit(&DeployCommit {
                namespace: namespace.clone(),
                removed_services,
                releases: committed_releases,
                deploy: deploy_record.clone(),
            })
            .await?;
        events.push(DeployEvent {
            step: "commit".into(),
            message: format!("committed deploy {} for '{}'", deploy_id, namespace),
        });

        let active_instance_ids: BTreeSet<String> = committed_slots
            .iter()
            .map(|slot| slot.active_instance_id.0.clone())
            .collect();
        let participant_ids: BTreeSet<String> = final_preview
            .participants
            .iter()
            .map(|machine_id| machine_id.0.clone())
            .collect();
        let mut cleanup_errors = Vec::new();

        for status in deploy_read.list_instance_status(namespace).await? {
            if active_instance_ids.contains(&status.instance_id.0) {
                continue;
            }
            if !participant_ids.contains(&status.machine_id.0) {
                continue;
            }
            let Some(session) = sessions.get_mut(&status.machine_id) else {
                continue;
            };
            if let Err(error) = session.drain_instance(&status.instance_id).await {
                cleanup_errors.push(error.to_string());
                continue;
            }
            match session.remove_instance(&status.instance_id).await {
                Ok(()) => events.push(DeployEvent {
                    step: "cleanup".into(),
                    message: format!(
                        "removed old instance {} from {}",
                        status.instance_id, status.machine_id
                    ),
                }),
                Err(error) => cleanup_errors.push(error.to_string()),
            }
        }

        let final_state = if cleanup_errors.is_empty() {
            DeployState::Committed
        } else {
            deploy_record.state = DeployState::CleanupPending;
            deploy_record.finished_at = Some(now_unix_secs());
            deploy_write.upsert_deploy(&deploy_record).await?;
            for error in cleanup_errors {
                events.push(DeployEvent {
                    step: "cleanup_pending".into(),
                    message: error,
                });
            }
            DeployState::CleanupPending
        };

        Ok(DeployApplyResult {
            deploy_id: deploy_id.clone(),
            preview: final_preview,
            state: final_state,
            events,
        })
    }
    .await;

    for (_machine_id, session) in sessions {
        let _ = session.close().await;
    }

    result
}

fn deployable_machines(
    machines: &[crate::model::MachineRecord],
    local_machine_id: &MachineId,
    now: u64,
) -> Vec<MachineId> {
    let mut enabled: Vec<MachineId> = machines
        .iter()
        .filter(|machine| machine.participation == crate::model::Participation::Enabled)
        .filter(|machine| machine_is_fresh(machine, now))
        .map(|machine| machine.id.clone())
        .collect();
    enabled.sort_by(|left, right| left.0.cmp(&right.0));
    if enabled.is_empty() {
        return vec![local_machine_id.clone()];
    }
    enabled
}

fn desired_slots(
    spec: &ServiceSpec,
    machines: &[MachineId],
    current_slots: Option<&[ServiceReleaseSlot]>,
) -> Result<Vec<DesiredSlot>> {
    let candidates = if machines.is_empty() {
        vec![MachineId("local".into())]
    } else {
        machines.to_vec()
    };
    if candidates.is_empty() {
        return Err(Error::operation(
            "desired_slots",
            "slot placement requires at least one candidate machine",
        ));
    }

    let mut desired = Vec::new();
    match spec.placement {
        Placement::Replicated { count } => {
            if count == 0 {
                return Err(Error::operation(
                    "desired_slots",
                    format!("service '{}' requested zero replicas", spec.name),
                ));
            }
            for index in 0..count {
                let slot_id = SlotId(format!("slot-{number:04}", number = usize::from(index) + 1));
                let machine_id = if let Some(machine_id) = current_slots.and_then(|slots| {
                    slots
                        .iter()
                        .find(|slot| slot.slot_id == slot_id)
                        .map(|slot| slot.machine_id.clone())
                }) {
                    machine_id
                } else {
                    let candidate_index = usize::from(index) % candidates.len();
                    let Some(machine_id) = candidates.get(candidate_index) else {
                        return Err(Error::operation(
                            "desired_slots",
                            format!("candidate index {candidate_index} out of bounds"),
                        ));
                    };
                    machine_id.clone()
                };
                desired.push(DesiredSlot {
                    slot_id,
                    machine_id,
                });
            }
        }
        Placement::Global => {
            for machine_id in &candidates {
                desired.push(DesiredSlot {
                    slot_id: SlotId(format!("slot-{}", machine_id.0)),
                    machine_id: machine_id.clone(),
                });
            }
        }
    }
    Ok(desired)
}

fn current_slots_by_service(
    current_releases: &HashMap<String, ServiceReleaseRecord>,
) -> HashMap<String, Vec<ServiceReleaseSlot>> {
    current_releases
        .iter()
        .map(|(service, release)| (service.clone(), release.release.slots.clone()))
        .collect()
}

fn current_slots_by_service_from_releases(
    current_releases: &[ServiceReleaseRecord],
) -> HashMap<String, Vec<ServiceReleaseSlot>> {
    current_releases
        .iter()
        .map(|release| (release.service.clone(), release.release.slots.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        DeployState, DrainState, InstanceId, InstancePhase, MachineRecord, MachineStatus,
        OverlayIp, Participation, PublicKey, ServiceReleaseSlot,
    };
    use async_trait::async_trait;
    use ployz_runtime_api::{DeploySession, DeploySessionFactory, StartCandidateRequest};
    use ployz_store_api::{DeployReadStore, MachineStore, memory::MemoryStore};
    use ployz_types::spec::{
        ContainerSpec, Namespace, NetworkMode, PullPolicy, Resources, RestartPolicy,
        RolloutStrategy,
    };
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;
    use std::sync::{Arc, Mutex};

    #[test]
    fn deployable_machines_excludes_stale_and_down_peers() {
        let now = 100;
        let machines = vec![
            test_machine(
                "fresh-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                90,
            ),
            test_machine(
                "stale-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                69,
            ),
            test_machine(
                "down-enabled",
                Participation::Enabled,
                MachineStatus::Down,
                100,
            ),
            test_machine(
                "draining-fresh",
                Participation::Draining,
                MachineStatus::Up,
                100,
            ),
        ];

        let deployable = deployable_machines(&machines, &MachineId("local".into()), now);
        assert_eq!(deployable, vec![MachineId("fresh-enabled".into())]);
    }

    #[test]
    fn deployable_machines_falls_back_to_local_when_none_are_fresh_enabled() {
        let machines = vec![
            test_machine(
                "stale-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                10,
            ),
            test_machine(
                "down-enabled",
                Participation::Enabled,
                MachineStatus::Down,
                100,
            ),
        ];

        let deployable = deployable_machines(&machines, &MachineId("local".into()), 100);
        assert_eq!(deployable, vec![MachineId("local".into())]);
    }

    #[test]
    fn replicated_one_reuses_existing_slot_machine() {
        let spec = ServiceSpec {
            name: "api".into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: "nginx:latest".into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                volumes: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: Vec::new(),
            publish: Vec::new(),
            routes: Vec::new(),
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            stop_grace_period: None,
            restart: RestartPolicy::UnlessStopped,
        };
        let machines = vec![MachineId("machine-a".into()), MachineId("machine-b".into())];
        let current_slots = [ServiceReleaseSlot {
            slot_id: SlotId("slot-0001".into()),
            machine_id: MachineId("machine-b".into()),
            active_instance_id: InstanceId("inst-1".into()),
            revision_hash: "rev-1".into(),
        }];

        let desired = desired_slots(&spec, &machines, Some(&current_slots)).expect("desired slots");
        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].slot_id, SlotId("slot-0001".into()));
        assert_eq!(desired[0].machine_id, MachineId("machine-b".into()));
    }

    #[tokio::test]
    async fn preview_returns_error_when_manifest_contains_duplicate_services() {
        let store = Arc::new(MemoryStore::new());
        let local_machine = live_machine("local");
        store
            .upsert_self_machine(&local_machine)
            .await
            .expect("seed local machine");
        let manifest = DeployManifest {
            namespace: Namespace("prod".into()),
            services: vec![
                test_service_spec("api", "nginx:1.27"),
                test_service_spec("api", "nginx:1.28"),
            ],
        };

        let error = preview(store.as_ref(), store.as_ref(), &local_machine.id, &manifest)
            .await
            .expect_err("preview should reject duplicate service names");

        assert!(error.to_string().contains("duplicate service 'api'"));
    }

    #[tokio::test]
    async fn apply_persists_commit_and_deploy_record() {
        let store = Arc::new(MemoryStore::new());
        let local_machine = live_machine("local");
        store
            .upsert_self_machine(&local_machine)
            .await
            .expect("seed local machine");
        let manifest = test_manifest("nginx:1.27");
        let session_factory = TestDeploySessionFactory::default();

        let result = apply(
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            &session_factory,
            &local_machine.id,
            &manifest,
        )
        .await
        .expect("apply should succeed");

        assert_eq!(result.state, DeployState::Committed);

        let stored_deploy = store
            .get_deploy(&result.deploy_id)
            .await
            .expect("load deploy record")
            .expect("deploy record should exist");
        assert_eq!(stored_deploy.state, DeployState::Committed);

        let releases = store
            .list_service_releases(&manifest.namespace)
            .await
            .expect("load releases");
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].service, "api");
    }

    #[tokio::test]
    async fn apply_returns_error_when_participants_change_after_locking() {
        let store = Arc::new(MemoryStore::new());
        let local_machine = live_machine("local");
        store
            .upsert_self_machine(&local_machine)
            .await
            .expect("seed local machine");
        let manifest = test_manifest("nginx:1.27");
        let session_factory = TestDeploySessionFactory::with_participant_drift(
            Arc::clone(&store),
            live_machine("peer-2"),
        );

        let error = apply(
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            &session_factory,
            &local_machine.id,
            &manifest,
        )
        .await
        .expect_err("apply should fail after participant drift");

        assert!(
            error
                .to_string()
                .contains("participant set changed after lock acquisition")
        );
    }

    fn test_machine(
        id: &str,
        participation: Participation,
        status: MachineStatus,
        last_heartbeat: u64,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status,
            participation,
            last_heartbeat,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    fn live_machine(id: &str) -> MachineRecord {
        test_machine(
            id,
            Participation::Enabled,
            MachineStatus::Up,
            now_unix_secs(),
        )
    }

    fn test_manifest(image: &str) -> DeployManifest {
        DeployManifest {
            namespace: Namespace("prod".into()),
            services: vec![test_service_spec("api", image)],
        }
    }

    fn test_service_spec(name: &str, image: &str) -> ServiceSpec {
        ServiceSpec {
            name: name.into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: image.into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                volumes: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: Vec::new(),
            publish: Vec::new(),
            routes: Vec::new(),
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            stop_grace_period: None,
            restart: RestartPolicy::UnlessStopped,
        }
    }

    #[derive(Default)]
    struct TestDeploySessionFactory {
        participant_drift: Option<(Arc<MemoryStore>, MachineRecord)>,
        drift_injected: Mutex<bool>,
    }

    impl TestDeploySessionFactory {
        fn with_participant_drift(store: Arc<MemoryStore>, machine: MachineRecord) -> Self {
            Self {
                participant_drift: Some((store, machine)),
                drift_injected: Mutex::new(false),
            }
        }
    }

    #[async_trait]
    impl DeploySessionFactory for TestDeploySessionFactory {
        async fn open(
            &self,
            machine: &MachineRecord,
            _namespace: &Namespace,
            _deploy_id: &DeployId,
            _coordinator_id: &MachineId,
        ) -> ployz_types::Result<(
            Box<dyn DeploySession>,
            Vec<crate::model::InstanceStatusRecord>,
        )> {
            let should_inject_drift = if self.participant_drift.is_some() {
                let mut drift_injected = self
                    .drift_injected
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *drift_injected {
                    false
                } else {
                    *drift_injected = true;
                    true
                }
            } else {
                false
            };

            if should_inject_drift && let Some((store, drift_machine)) = &self.participant_drift {
                store
                    .upsert_self_machine(drift_machine)
                    .await
                    .expect("inject participant drift");
            }

            Ok((
                Box::new(TestDeploySession {
                    machine_id: machine.id.clone(),
                }),
                Vec::new(),
            ))
        }
    }

    struct TestDeploySession {
        machine_id: MachineId,
    }

    #[async_trait]
    impl DeploySession for TestDeploySession {
        fn machine_id(&self) -> &MachineId {
            &self.machine_id
        }

        async fn inspect_namespace(
            &mut self,
        ) -> ployz_types::Result<Vec<crate::model::InstanceStatusRecord>> {
            Ok(Vec::new())
        }

        async fn start_candidate(
            &mut self,
            request: StartCandidateRequest,
        ) -> ployz_types::Result<crate::model::InstanceStatusRecord> {
            let spec: ServiceSpec =
                serde_json::from_str(&request.spec_json).expect("valid spec json in test");
            let revision_hash = spec.revision_hash().expect("revision hash");

            Ok(crate::model::InstanceStatusRecord {
                instance_id: request.instance_id,
                namespace: Namespace("prod".into()),
                service: request.service,
                slot_id: request.slot_id,
                machine_id: self.machine_id.clone(),
                revision_hash,
                deploy_id: DeployId("deploy-under-test".into()),
                docker_container_id: "container-under-test".into(),
                overlay_ip: None,
                backend_ports: BTreeMap::new(),
                phase: InstancePhase::Ready,
                ready: true,
                drain_state: DrainState::None,
                error: None,
                started_at: now_unix_secs(),
                updated_at: now_unix_secs(),
            })
        }

        async fn drain_instance(&mut self, _instance_id: &InstanceId) -> ployz_types::Result<()> {
            Ok(())
        }

        async fn remove_instance(&mut self, _instance_id: &InstanceId) -> ployz_types::Result<()> {
            Ok(())
        }

        async fn close(self: Box<Self>) -> ployz_types::Result<()> {
            Ok(())
        }
    }
}
