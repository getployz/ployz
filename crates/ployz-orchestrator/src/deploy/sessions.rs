use crate::error::{Error, Result};
use crate::model::{DeployEvent, DeployId, DeployPreview, MachineId};
use ployz_runtime_api::{DeploySession, DeploySessionFactory};
use ployz_types::spec::Namespace;
use std::collections::{BTreeMap, HashMap};

pub(crate) async fn open_sessions(
    machine_map: &HashMap<MachineId, crate::model::MachineRecord>,
    participants: &[MachineId],
    session_factory: &dyn DeploySessionFactory,
    namespace: &Namespace,
    deploy_id: &DeployId,
    local_machine_id: &MachineId,
) -> Result<(
    BTreeMap<MachineId, Box<dyn DeploySession>>,
    Vec<DeployEvent>,
)> {
    let mut events = Vec::new();
    let mut sessions = BTreeMap::new();

    for participant in participants {
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
            .open(machine, namespace, deploy_id, local_machine_id)
            .await
            .map_err(|error| Error::operation("deploy_apply", error.to_string()))?;
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

    Ok((sessions, events))
}

pub(crate) fn ensure_participants_stable(
    initial: &DeployPreview,
    final_preview: &DeployPreview,
) -> Result<()> {
    if final_preview.participants != initial.participants {
        return Err(Error::operation(
            "deploy_apply",
            "participant set changed after lock acquisition; retry deploy",
        ));
    }
    Ok(())
}

pub(crate) async fn close_sessions(sessions: BTreeMap<MachineId, Box<dyn DeploySession>>) {
    for (_machine_id, session) in sessions {
        let _ = session.close().await;
    }
}
