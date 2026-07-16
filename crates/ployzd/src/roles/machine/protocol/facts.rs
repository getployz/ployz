use ployz_core::ids::MachineId;
use ployz_core::machine::runtime::{MachineFactsRefreshConfirmation, MachineFactsSnapshot};
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};

use super::{MachineRpcResponder, MachineRpcResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsGetRpcRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsGetRpcOk {
    pub facts: MachineFactsSnapshot,
    /// Point-of-use testimony that this process can accept `BuildStart`.
    ///
    /// The alpha machine RPC is intentionally strict: peers running a wire
    /// version without this required field are incompatible rather than being
    /// treated as build-capable by default.
    pub build: MachineBuildCapability,
}

impl MachineRpcResponder for MachineFactsGetRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { facts, build: _ } = self;
        facts.machine_id()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineBuildCapability {
    Available,
    Unavailable,
}

pub type MachineFactsGetRpcResponse =
    MachineRpcResponse<MachineFactsGetRpcOk, MachineFactsGetDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineFactsGetDomainError {
    GatherFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsRefreshRpcRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsRefreshRpcOk {
    pub refresh: MachineFactsRefreshConfirmation,
}

impl MachineRpcResponder for MachineFactsRefreshRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self {
            refresh:
                MachineFactsRefreshConfirmation {
                    machine_id,
                    observed_at_unix_ms: _,
                },
        } = self;
        machine_id
    }
}

pub type MachineFactsRefreshRpcResponse =
    MachineRpcResponse<MachineFactsRefreshRpcOk, MachineFactsRefreshDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineFactsRefreshDomainError {
    RefreshFailed { message: FailureMessage },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::image::OciPlatform;
    use ployz_core::machine::runtime::{MachineContainerObservationSnapshot, MachineDiskSpace};

    fn facts() -> MachineFactsSnapshot {
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, []).expect("containers"),
            None,
            MachineDiskSpace {
                available_bytes: 1,
                total_bytes: 2,
            },
            None,
            OciPlatform::try_new("linux", "amd64").expect("platform"),
            1,
        )
        .expect("facts")
    }

    #[test]
    fn facts_get_carries_both_build_capability_values() {
        for build in [
            MachineBuildCapability::Available,
            MachineBuildCapability::Unavailable,
        ] {
            let response = MachineFactsGetRpcOk {
                facts: facts(),
                build,
            };
            let encoded = serde_json::to_value(&response).expect("encode");
            assert_eq!(
                encoded.get("build").expect("build field"),
                &serde_json::to_value(build).expect("value")
            );
            assert_eq!(
                serde_json::from_value::<MachineFactsGetRpcOk>(encoded).expect("decode"),
                response
            );
        }
    }

    #[test]
    fn facts_get_rejects_pre_capability_alpha_responses() {
        let encoded = serde_json::json!({ "facts": facts() });
        assert!(serde_json::from_value::<MachineFactsGetRpcOk>(encoded).is_err());
    }
}
