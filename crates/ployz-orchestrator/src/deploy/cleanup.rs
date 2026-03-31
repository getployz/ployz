use crate::error::Result;
use crate::model::{
    DeployEvent, DeployPreview, DeployRecord, DeployState, MachineId, ServiceReleaseSlot,
};
use ployz_runtime_api::DeploySession;
use ployz_store_api::{DeployReadStore, DeployWriteStore};
use ployz_types::time::now_unix_secs;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct CleanupOutcome {
    pub(crate) final_state: DeployState,
    pub(crate) events: Vec<DeployEvent>,
}

pub(crate) async fn cleanup_stale_instances(
    deploy_read: &dyn DeployReadStore,
    deploy_write: &dyn DeployWriteStore,
    sessions: &mut BTreeMap<MachineId, Box<dyn DeploySession>>,
    final_preview: &DeployPreview,
    committed_slots: &[ServiceReleaseSlot],
    deploy_record: &mut DeployRecord,
) -> Result<CleanupOutcome> {
    let active_instance_ids: BTreeSet<String> = committed_slots
        .iter()
        .map(|slot| slot.active_instance_id.0.clone())
        .collect();
    let participant_ids: BTreeSet<String> = final_preview
        .participants
        .iter()
        .map(|machine_id| machine_id.0.clone())
        .collect();

    let mut events = Vec::new();
    let mut cleanup_errors = Vec::new();

    for status in deploy_read
        .list_instance_status(&final_preview.namespace)
        .await?
    {
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
        deploy_write.upsert_deploy(deploy_record).await?;
        for error in cleanup_errors {
            events.push(DeployEvent {
                step: "cleanup_pending".into(),
                message: error,
            });
        }
        DeployState::CleanupPending
    };

    Ok(CleanupOutcome {
        final_state,
        events,
    })
}
