//! Data Loss identity.

use ployz_core::{
    AmbiguousDataLossName, DataLoss, DockerVolumeId, DockerVolumeName, MachineId, ObservedDataLoss,
    RpcError, RpcErrorCode, UnconfirmedDataLoss,
};
use serde_json::json;

#[test]
fn docker_volume_data_loss_carries_machine_and_name() {
    let loss = DataLoss::DockerVolume(DockerVolumeId {
        machine_id: machine_id('a'),
        name: DockerVolumeName::parse("data").unwrap(),
    });
    let encoded = serde_json::to_value(&loss).unwrap();
    assert_eq!(
        encoded,
        json!({
            "DockerVolume": {
                "machine_id": machine_id('a').as_str(),
                "name": "data"
            }
        })
    );
    assert!(encoded.get("kind").is_none());
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
        "DockerVolume": { "name": "data" }
    }))
    .unwrap_err();
}

#[test]
fn uncovered_by_is_empty_when_confirmation_names_every_observed_loss() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('a', "logs")],
    };
    assert_eq!(
        observed.uncovered_by(&[volume('a', "logs"), volume('a', "data")]),
        Vec::<DataLoss>::new()
    );
}

#[test]
fn uncovered_by_names_fresh_loss_the_confirmation_omitted() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('a', "logs")],
    };
    assert_eq!(
        observed.uncovered_by(&[volume('a', "data")]),
        vec![volume('a', "logs")]
    );
}

#[test]
fn uncovered_by_ignores_confirmed_names_that_are_no_longer_observed() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data")],
    };
    assert_eq!(
        observed.uncovered_by(&[volume('a', "data"), volume('a', "gone")]),
        Vec::<DataLoss>::new()
    );
}

#[test]
fn uncovered_by_allows_an_empty_confirmation_when_there_is_no_data_loss() {
    let observed = ObservedDataLoss {
        data_loss: Vec::new(),
    };
    assert_eq!(observed.uncovered_by(&[]), Vec::<DataLoss>::new());
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
fn named_resolves_unique_display_names_to_listed_identities() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('a', "logs")],
    };
    assert_eq!(
        observed.named(["logs", "data"]).unwrap(),
        vec![volume('a', "logs"), volume('a', "data")]
    );
}

#[test]
fn named_ignores_display_names_that_match_no_listed_entry() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data")],
    };
    assert_eq!(
        observed.named(["data", "gone"]).unwrap(),
        vec![volume('a', "data")]
    );
}

#[test]
fn named_refuses_a_display_name_that_matches_more_than_one_listed_entry() {
    let observed = ObservedDataLoss {
        data_loss: vec![volume('a', "data"), volume('b', "data")],
    };
    assert_eq!(
        observed.named(["data"]).unwrap_err(),
        AmbiguousDataLossName {
            name: "data".into()
        }
    );
}

#[test]
fn named_allows_an_empty_list_when_there_is_no_data_loss() {
    let observed = ObservedDataLoss {
        data_loss: Vec::new(),
    };
    assert_eq!(
        observed.named([] as [&str; 0]).unwrap(),
        Vec::<DataLoss>::new()
    );
}

fn volume(machine: char, name: &str) -> DataLoss {
    DataLoss::DockerVolume(DockerVolumeId {
        machine_id: machine_id(machine),
        name: DockerVolumeName::parse(name).unwrap(),
    })
}

fn machine_id(value: char) -> MachineId {
    MachineId::parse(value.to_string().repeat(32)).unwrap()
}
