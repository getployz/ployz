use crate::error::{DeployError, Error, Result};
use crate::model::{
    DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineMembership, SlotId,
};
use ployz_types::spec::Namespace;

#[async_trait::async_trait]
pub trait DeployParticipantClient: Send + Sync {
    fn supports_volume_moves(&self) -> bool {
        false
    }

    fn supports_volume_clones(&self) -> bool {
        false
    }

    async fn inspect_namespace(
        &self,
        machine: &MachineMembership,
        namespace: &Namespace,
        deploy_id: &DeployId,
        coordinator_id: &MachineId,
    ) -> Result<Vec<InstanceStatusRecord>>;

    async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord>;

    async fn move_volume(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: MoveVolumeRequest,
    ) -> Result<MoveVolumeResult> {
        Err(Error::Deploy(DeployError::VolumeMoveExecutionUnsupported {
            volume: request.volume,
        }))
    }

    async fn clone_volume(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: CloneVolumeRequest,
    ) -> Result<CloneVolumeResult> {
        Err(Error::Deploy(
            DeployError::VolumeCloneExecutionUnsupported {
                volume: request.volume,
            },
        ))
    }

    async fn cleanup_volume_clone(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _request: CleanupVolumeCloneRequest,
    ) -> Result<()> {
        Ok(())
    }

    async fn drain_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<()>;

    async fn remove_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct StartCandidateRequest {
    pub service: String,
    pub slot_id: SlotId,
    pub instance_id: InstanceId,
    pub spec_json: String,
    pub volumes_json: String,
}

#[derive(Debug, Clone)]
pub struct MoveVolumeRequest {
    pub volume: String,
    pub from_machine: MachineId,
    pub to_machine: MachineId,
    pub snapshot: String,
}

#[derive(Debug, Clone)]
pub struct MoveVolumeResult {
    pub snapshot: String,
    pub snapshot_guid: u64,
    pub bytes_transferred: u64,
}

#[derive(Debug, Clone)]
pub struct CloneVolumeRequest {
    pub volume: String,
    pub source_namespace: Namespace,
    pub source_volume: String,
    pub snapshot: String,
    pub quota: String,
    pub mode: String,
    pub owner: String,
}

#[derive(Debug, Clone)]
pub struct CloneVolumeResult {
    pub snapshot: String,
    pub snapshot_guid: u64,
    pub target_dataset: String,
}

#[derive(Debug, Clone)]
pub struct CleanupVolumeCloneRequest {
    pub volume: String,
    pub source_namespace: Namespace,
    pub source_volume: String,
    pub snapshot: String,
}
