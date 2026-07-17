use super::{MachineRpcResponder, MachineRpcResponse};
use ployz_core::ids::MachineId;
use ployz_core::intent::VolumePinState;
use ployz_core::machine::VolumeUsageFacts;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineVolumeTestimonyRpcRequest {
    pub pins: Vec<VolumePinState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineVolumeTestimonyResult {
    pub pin: VolumePinState,
    pub testimony: MachineVolumeTestimony,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineVolumeTestimony {
    Available { facts: VolumeUsageFacts },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineVolumeTestimonyRpcOk {
    pub machine_id: MachineId,
    pub results: Vec<MachineVolumeTestimonyResult>,
}

impl MachineRpcResponder for MachineVolumeTestimonyRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self {
            machine_id,
            results: _,
        } = self;
        machine_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineVolumeTestimonyDomainError {
    MachineMismatch {
        expected_machine_id: MachineId,
        responder_machine_id: MachineId,
    },
}

pub type MachineVolumeTestimonyRpcResponse =
    MachineRpcResponse<MachineVolumeTestimonyRpcOk, MachineVolumeTestimonyDomainError>;

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::VolumeName;
    use ployz_core::ids::NamespaceId;
    use serde_json::json;

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn plain_volume_pin(volume: &str) -> VolumePinState {
        VolumePinState::plain(
            NamespaceId::try_new("default").expect("namespace"),
            VolumeName::try_new(volume).expect("volume"),
            machine_id("machine_a"),
        )
    }

    #[test]
    fn wire_exposes_only_pin_and_normalized_facts() {
        let response = MachineVolumeTestimonyRpcResponse::Ok(MachineVolumeTestimonyRpcOk {
            machine_id: machine_id("machine_a"),
            results: vec![
                MachineVolumeTestimonyResult {
                    pin: plain_volume_pin("available"),
                    testimony: MachineVolumeTestimony::Available {
                        facts: VolumeUsageFacts {
                            used_bytes: 4096,
                            last_write_unix_seconds: 1_700_000_000,
                        },
                    },
                },
                MachineVolumeTestimonyResult {
                    pin: plain_volume_pin("unavailable"),
                    testimony: MachineVolumeTestimony::Unavailable,
                },
            ],
        });

        let encoded = serde_json::to_value(&response).expect("response serializes");
        assert_eq!(
            encoded,
            json!({
                "status": "ok",
                "machine_id": "machine_a",
                "results": [
                    {
                        "pin": {
                            "namespace_id": "default",
                            "volume_name": "available",
                            "machine_id": "machine_a",
                            "kind": { "kind": "plain" }
                        },
                        "testimony": {
                            "state": "available",
                            "facts": {
                                "used_bytes": 4096,
                                "last_write_unix_seconds": 1700000000
                            }
                        }
                    },
                    {
                        "pin": {
                            "namespace_id": "default",
                            "volume_name": "unavailable",
                            "machine_id": "machine_a",
                            "kind": { "kind": "plain" }
                        },
                        "testimony": { "state": "unavailable" }
                    }
                ]
            })
        );
        assert_eq!(
            serde_json::from_value::<MachineVolumeTestimonyRpcResponse>(encoded)
                .expect("response deserializes"),
            response
        );
    }
}
