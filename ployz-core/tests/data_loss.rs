//! Data Loss identity.

use ployz_core::{
    DataLoss, DockerVolumeId, DockerVolumeName, MachineId, ObservedDataLoss, RpcError,
    RpcErrorCode, UnconfirmedDataLoss,
};
use serde_json::json;

#[test]
fn docker_volume_data_loss_carries_machine_and_name() {
    let loss = DataLoss::DockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id('a'),
            name: DockerVolumeName::parse("data").unwrap(),
        },
    };
    let encoded = serde_json::to_value(&loss).unwrap();
    assert_eq!(
        encoded,
        json!({
            "kind": "docker_volume",
            "id": {
                "machine_id": machine_id('a').as_str(),
                "name": "data"
            }
        })
    );
    assert!(encoded.get("name").is_none());
    assert!(encoded.get("scope").is_none());
    assert_eq!(serde_json::from_value::<DataLoss>(encoded).unwrap(), loss);
}

#[test]
fn a_kind_cannot_carry_an_identity_that_does_not_belong_to_it() {
    let illegal = json!({
        "kind": "docker_volume",
        "name": "data",
        "scope": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    });
    serde_json::from_value::<DataLoss>(illegal).unwrap_err();
    serde_json::from_value::<DataLoss>(json!({
        "kind": "docker_volume",
        "id": { "name": "data" }
    }))
    .unwrap_err();
    serde_json::from_value::<DataLoss>(json!({
        "DockerVolume": { "machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "name": "data" }
    }))
    .unwrap_err();
}

#[test]
fn confirmation_serializes_as_a_distinct_exact_identity_set() {
    let observed = ObservedDataLoss {
        data_loss: vec![
            volume('a', "data"),
            volume('a', "data"),
            volume('a', "logs"),
        ],
    };
    let confirmation = observed.confirm_names(["logs", "data"]).unwrap();

    assert_eq!(
        serde_json::to_value(confirmation).unwrap(),
        json!({ "confirmed": [volume('a', "data"), volume('a', "logs")] })
    );
}

#[test]
fn require_names_fresh_loss_the_confirmation_omitted() {
    let reviewed = ObservedDataLoss {
        data_loss: vec![volume('a', "data")],
    };
    let confirmation = reviewed.confirm_names(["data"]).unwrap();
    let fresh = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('a', "logs")],
    };

    assert_eq!(
        fresh.require(&confirmation).unwrap_err(),
        UnconfirmedDataLoss {
            missing: vec![volume('a', "logs")]
        }
    );
}

#[test]
fn require_ignores_confirmed_loss_that_is_no_longer_observed() {
    let reviewed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('a', "gone")],
    };
    let confirmation = reviewed.confirm_names(["data", "gone"]).unwrap();
    let fresh = ObservedDataLoss {
        data_loss: vec![volume('a', "data")],
    };

    assert!(fresh.require(&confirmation).is_ok());
}

#[test]
fn an_empty_observation_accepts_an_empty_confirmation() {
    let observed = ObservedDataLoss {
        data_loss: Vec::new(),
    };

    assert!(observed.confirm_names([] as [&str; 0]).is_ok());
}

#[test]
fn unconfirmed_data_loss_names_missing_identities_in_the_rpc_error() {
    let missing = vec![volume('a', "logs")];
    let error = UnconfirmedDataLoss {
        missing: missing.clone(),
    }
    .into_rpc_error();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        format!(
            "Data Loss is not covered by the confirmation: logs on {}",
            machine_id('a')
        )
    );
    assert_eq!(
        UnconfirmedDataLoss::from_rpc_error(&error).unwrap().missing,
        missing
    );
    let details: UnconfirmedDataLoss = serde_json::from_value(error.details).unwrap();
    assert_eq!(details.missing, missing);
}

#[test]
fn from_rpc_error_is_none_when_the_error_is_not_unconfirmed_data_loss() {
    assert!(
        UnconfirmedDataLoss::from_rpc_error(&RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: "Machine was not found".into(),
            details: json!(null),
        })
        .is_none()
    );
    assert!(
        UnconfirmedDataLoss::from_rpc_error(&RpcError {
            code: RpcErrorCode::NotFound,
            message: "gone".into(),
            details: json!({ "missing": [] }),
        })
        .is_none()
    );
}

#[test]
fn docker_volume_data_loss_display_is_name_on_machine() {
    assert_eq!(
        volume('a', "data").to_string(),
        format!("data on {}", machine_id('a'))
    );
}

#[test]
fn docker_volume_data_loss_name_is_the_volume_name() {
    assert_eq!(volume('a', "data").name(), "data");
}

#[test]
fn confirm_names_resolves_unique_display_names_to_listed_identities() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('a', "logs")],
    };
    assert_eq!(
        serde_json::to_value(observed.confirm_names(["logs", "data"]).unwrap()).unwrap(),
        json!({ "confirmed": [volume('a', "data"), volume('a', "logs")] })
    );
}

#[test]
fn confirm_names_ignores_display_names_that_match_no_listed_entry() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data")],
    };
    assert_eq!(
        serde_json::to_value(observed.confirm_names(["data", "gone"]).unwrap()).unwrap(),
        json!({ "confirmed": [volume('a', "data")] })
    );
}

#[test]
fn confirm_names_returns_every_observed_identity_the_names_omit() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('a', "logs")],
    };

    assert_eq!(
        observed.confirm_names(["data"]).unwrap_err(),
        UnconfirmedDataLoss {
            missing: vec![volume('a', "logs")]
        }
    );
}

#[test]
fn one_name_confirms_every_observed_volume_with_that_name() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('b', "data")],
    };
    let confirmation = observed.confirm_names(["data"]).unwrap();

    assert!(observed.require(&confirmation).is_ok());
}

fn volume(machine: char, name: &str) -> DataLoss {
    DataLoss::DockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        },
    }
}

fn machine_id(value: char) -> MachineId {
    MachineId::parse(value.to_string().repeat(32)).unwrap()
}
