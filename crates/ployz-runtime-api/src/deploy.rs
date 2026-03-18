use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineRecord, SlotId,
};
use ployz_types::spec::Namespace;
use serde::{Deserialize, Serialize};

#[async_trait]
pub trait DeploySessionFactory: Send + Sync {
    async fn open(
        &self,
        machine: &MachineRecord,
        namespace: &Namespace,
        deploy_id: &DeployId,
        coordinator_id: &MachineId,
    ) -> Result<(Box<dyn DeploySession>, Vec<InstanceStatusRecord>)>;
}

#[async_trait]
pub trait DeploySession: Send {
    fn machine_id(&self) -> &MachineId;

    async fn inspect_namespace(&mut self) -> Result<Vec<InstanceStatusRecord>>;

    async fn start_candidate(
        &mut self,
        request: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord>;

    async fn drain_instance(&mut self, instance_id: &InstanceId) -> Result<()>;

    async fn remove_instance(&mut self, instance_id: &InstanceId) -> Result<()>;

    async fn close(self: Box<Self>) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct StartCandidateRequest {
    pub service: String,
    pub slot_id: SlotId,
    pub instance_id: InstanceId,
    pub spec_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeployFrame {
    Open {
        namespace: String,
        deploy_id: String,
        coordinator_id: String,
    },
    InspectNamespace,
    StartCandidate {
        service: String,
        slot_id: String,
        instance_id: String,
        spec_json: String,
    },
    DrainInstance {
        instance_id: String,
    },
    RemoveInstance {
        instance_id: String,
    },
    Close,
    Opened {
        instances: Vec<InstanceStatusRecord>,
    },
    NamespaceSnapshot {
        instances: Vec<InstanceStatusRecord>,
    },
    CandidateStarted {
        status: Box<InstanceStatusRecord>,
    },
    Ack {
        message: String,
    },
    Error {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::DeployFrame;

    #[test]
    fn start_candidate_roundtrip_is_session_scoped() {
        let frame = DeployFrame::StartCandidate {
            service: String::from("api"),
            slot_id: String::from("slot-1"),
            instance_id: String::from("inst-1"),
            spec_json: String::from("{\"name\":\"api\"}"),
        };

        let json = serde_json::to_value(&frame).expect("serialize frame");
        let start_candidate = json
            .get("StartCandidate")
            .expect("enum variant payload should exist");

        assert!(start_candidate.get("deploy_id").is_none());

        let decoded: DeployFrame = serde_json::from_value(json).expect("deserialize frame");
        let DeployFrame::StartCandidate {
            service,
            slot_id,
            instance_id,
            spec_json,
        } = decoded
        else {
            panic!("unexpected frame");
        };
        assert_eq!(service, "api");
        assert_eq!(slot_id, "slot-1");
        assert_eq!(instance_id, "inst-1");
        assert_eq!(spec_json, "{\"name\":\"api\"}");
    }
}
