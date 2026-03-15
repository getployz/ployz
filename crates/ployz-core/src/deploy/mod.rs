pub mod local;
pub mod locks;
pub mod remote;
pub mod session;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;

use crate::StoreDriver;
use crate::error::{Error, Result};
use crate::machine_liveness::machine_is_fresh;
use crate::model::{
    DeployApplyResult, DeployChangeKind, DeployEvent, DeployId, DeployPreview, DeployRecord,
    DeployState, InstanceId, MachineId, ServicePlan, ServiceRelease, ServiceReleaseRecord,
    ServiceReleaseSlot, ServiceRevisionRecord, ServiceRoutingPolicy, SlotId, SlotPlan,
};
use crate::spec::{DeployManifest, Placement, ServiceSpec};
use crate::store::{DeployStore, MachineStore};

pub use local::LocalDeployRuntime;
pub use locks::{NamespaceLock, NamespaceLockManager};

use local::{now_unix_secs, stable_hash_hex};

#[derive(Debug, Clone)]
struct DesiredSlot {
    slot_id: SlotId,
    machine_id: MachineId,
}

#[allow(clippy::indexing_slicing)]
pub async fn preview(
    store: &StoreDriver,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployPreview> {
    manifest
        .validate()
        .map_err(|e| Error::operation("deploy_preview", e))?;
    let namespace = &manifest.namespace;

    let current_releases = store.list_service_releases(namespace).await?;
    let machines = store.list_machines().await?;
    let desired_machines = deployable_machines(&machines, local_machine_id, now_unix_secs());
    let current_release_map: HashMap<String, ServiceReleaseRecord> = current_releases
        .into_iter()
        .map(|record| (record.service.clone(), record))
        .collect();
    let current_slots_by_service = current_slots_by_service(&current_release_map);

    let manifest_hash = stable_hash_hex(
        serde_json::to_vec(manifest)
            .map_err(|e| Error::operation("deploy_preview", format!("serialize manifest: {e}")))?
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
            .map_err(|e| Error::operation("deploy_preview", e))?;
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
            && current_release
                .map(|release| release.release.primary_revision_hash.as_str())
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

#[allow(clippy::indexing_slicing)]
pub async fn apply(
    store: &StoreDriver,
    session_factory: &dyn session::DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployApplyResult> {
    let namespace = &manifest.namespace;
    let deploy_id = DeployId(Uuid::new_v4().to_string());
    let started_at = now_unix_secs();
    let initial_preview = preview(store, local_machine_id, manifest).await?;
    let machines = store.list_machines().await?;
    let machine_map: HashMap<MachineId, crate::model::MachineRecord> = machines
        .iter()
        .map(|machine| (machine.id.clone(), machine.clone()))
        .collect();

    let mut events = vec![];

    // Open sessions for ALL participants (including self) in sorted order
    // to avoid deadlock if two coordinators try to lock the same set.
    let mut sorted_participants = initial_preview.participants.clone();
    sorted_participants.sort();

    let mut sessions: BTreeMap<MachineId, Box<dyn session::DeploySession>> = BTreeMap::new();

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
        let (sess, instances) = session_factory
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
        sessions.insert(participant.clone(), sess);
    }

    let result = async {
        let final_preview = preview(store, local_machine_id, manifest).await?;
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
            summary_json: serde_json::to_string(&final_preview)
                .map_err(|e| Error::operation("deploy_apply", format!("serialize preview: {e}")))?,
        };
        store.upsert_deploy(&deploy_record).await?;

        let current_slots_by_service =
            current_slots_by_service_from_releases(&store.list_service_releases(namespace).await?);
        let desired_machines = deployable_machines(&machines, local_machine_id, now_unix_secs());
        let mut removed_services = Vec::new();
        let mut committed_releases = Vec::new();
        let mut committed_slots = Vec::new();

        for spec in &manifest.services {
            let revision_hash = spec
                .revision_hash()
                .map_err(|e| Error::operation("deploy_apply", e))?;
            let spec_json = spec
                .canonical_revision_json()
                .map_err(|e| Error::operation("deploy_apply", e))?;
            store
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
                    let Some(sess) = sessions.get_mut(&desired_slot.machine_id) else {
                        return Err(Error::operation(
                            "deploy_apply",
                            format!(
                                "no session was available for machine '{}'",
                                desired_slot.machine_id
                            ),
                        ));
                    };
                    let status = sess
                        .start_candidate(session::StartCandidateRequest {
                            service: spec.name.clone(),
                            slot_id: desired_slot.slot_id.clone(),
                            instance_id,
                            spec_json: spec_json.clone(),
                        })
                        .await?;
                    store.upsert_instance_status(&status).await?;
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
                        revision_hash: revision_hash,
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
        deploy_record.summary_json = serde_json::to_string(&final_preview)
            .map_err(|e| Error::operation("deploy_apply", format!("serialize preview: {e}")))?;

        store
            .commit_deploy(
                namespace,
                &removed_services,
                &committed_releases,
                &deploy_record,
            )
            .await?;
        events.push(DeployEvent {
            step: "commit".into(),
            message: format!("committed deploy {} for '{}'", deploy_id, namespace),
        });

        // Cleanup: drain and remove old instances via uniform sessions.
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
        for status in store.list_instance_status(namespace).await? {
            if active_instance_ids.contains(&status.instance_id.0) {
                continue;
            }
            if !participant_ids.contains(&status.machine_id.0) {
                continue;
            }
            let Some(sess) = sessions.get_mut(&status.machine_id) else {
                continue;
            };
            if let Err(err) = sess.drain_instance(&status.instance_id).await {
                cleanup_errors.push(err.to_string());
                continue;
            }
            match sess.remove_instance(&status.instance_id).await {
                Ok(()) => {
                    if status.machine_id == *local_machine_id {
                        // Local removal already deleted from store via DeployAgent.
                    }
                    events.push(DeployEvent {
                        step: "cleanup".into(),
                        message: format!(
                            "removed old instance {} from {}",
                            status.instance_id, status.machine_id
                        ),
                    });
                }
                Err(err) => cleanup_errors.push(err.to_string()),
            }
        }

        let final_state = if cleanup_errors.is_empty() {
            DeployState::Committed
        } else {
            deploy_record.state = DeployState::CleanupPending;
            deploy_record.finished_at = Some(now_unix_secs());
            store.upsert_deploy(&deploy_record).await?;
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

    // Close all sessions (releases locks).
    for (_machine_id, sess) in sessions {
        let _ = sess.close().await;
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

#[allow(clippy::indexing_slicing)]
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
                let machine_id = current_slots
                    .and_then(|slots| {
                        slots.iter()
                            .find(|slot| slot.slot_id == slot_id)
                            .map(|slot| slot.machine_id.clone())
                    })
                    .unwrap_or_else(|| candidates[usize::from(index) % candidates.len()].clone());
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
    let mut grouped = HashMap::new();
    for release in current_releases {
        grouped.insert(release.service.clone(), release.release.slots.clone());
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        InstanceId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
        ServiceReleaseSlot,
    };
    use crate::spec::{ContainerSpec, NetworkMode, PullPolicy, Resources, RestartPolicy};
    use std::net::Ipv6Addr;

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
            rollout: crate::spec::RolloutStrategy::Recreate,
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
}
