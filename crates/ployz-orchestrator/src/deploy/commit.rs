use crate::deploy::cleanup::cleanup_stale_instances;
use crate::deploy::planning::{deployable_machines, desired_slots, preview};
use crate::deploy::sessions::{close_sessions, ensure_participants_stable, open_sessions};
use crate::error::{Error, Result};
use crate::model::{
    DeployApplyResult, DeployChangeKind, DeployEvent, DeployId, DeployRecord, DeployState,
    InstanceId, MachineId, ServiceRelease, ServiceReleaseRecord, ServiceReleaseSlot,
    ServiceRevisionRecord, ServiceRoutingPolicy,
};
use ployz_runtime_api::{DeploySessionFactory, StartCandidateRequest};
use ployz_store_api::{
    DeployCommit, DeployCommitStore, DeployReadStore, DeployWriteStore, MachineStore,
};
use ployz_types::spec::DeployManifest;
use ployz_types::time::now_unix_secs;
use std::collections::HashMap;
use uuid::Uuid;

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

    let mut sorted_participants = initial_preview.participants.clone();
    sorted_participants.sort();
    let (mut sessions, mut events) = open_sessions(
        &machine_map,
        &sorted_participants,
        session_factory,
        namespace,
        &deploy_id,
        local_machine_id,
    )
    .await?;

    let result = async {
        let final_preview = preview(deploy_read, machine_store, local_machine_id, manifest).await?;
        ensure_participants_stable(&initial_preview, &final_preview)?;

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
                        .await
                        .map_err(|error| Error::operation("deploy_apply", error.to_string()))?;
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

        let cleanup = cleanup_stale_instances(
            deploy_read,
            deploy_write,
            &mut sessions,
            &final_preview,
            &committed_slots,
            &mut deploy_record,
        )
        .await?;
        events.extend(cleanup.events);

        Ok(DeployApplyResult {
            deploy_id: deploy_id.clone(),
            preview: final_preview,
            state: cleanup.final_state,
            events,
        })
    }
    .await;

    close_sessions(sessions).await;
    result
}

fn current_slots_by_service_from_releases(
    current_releases: &[ServiceReleaseRecord],
) -> HashMap<String, Vec<ServiceReleaseSlot>> {
    current_releases
        .iter()
        .map(|release| (release.service.clone(), release.release.slots.clone()))
        .collect()
}
