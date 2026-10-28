use ployz_core::config::{SavedVolumeIntent, VolumeKind};

use super::*;

fn volume(id: &str, name: &str) -> SavedVolumeIntent {
    SavedVolumeIntent {
        resource_id: id.to_owned(),
        resource_lineage_id: id.to_owned(),
        name: name.to_owned(),
        storage: VolumeKind::Docker {},
        shared_writes: false,
        restored_from: None,
    }
}

fn holding(volumes: Vec<SavedVolumeIntent>) -> SavedEnvironmentIntent {
    SavedEnvironmentIntent {
        version: 1,
        environment_slug: "production".to_owned(),
        services: Vec::new(),
        volumes,
        configs: Vec::new(),
    }
}

#[test]
fn consent_for_a_restored_volume_does_not_carry_while_a_removed_one_keeps_it() {
    let data = volume("00000000-0000-4000-8000-000000000005", "data");
    let logs = volume("00000000-0000-4000-8000-000000000006", "logs");
    let namespace = Namespace::parse("shop-production").unwrap();
    let applied = holding(vec![data.clone(), logs]);
    let consent: Vec<VolumeLoss> = removed(&applied, &holding(Vec::new()), &namespace)
        .unwrap()
        .into_iter()
        .map(|volume| VolumeLoss {
            volume,
            deletes: Vec::new(),
        })
        .collect();

    let carried = still_removed(&consent, &holding(vec![data]));

    let names: Vec<&str> = carried
        .iter()
        .map(|loss| loss.volume.name.as_str())
        .collect();
    assert_eq!(names, ["logs"]);
}

fn removing_data() -> Vec<RemovedVolume> {
    let data = volume("00000000-0000-4000-8000-000000000005", "data");
    let namespace = Namespace::parse("shop-production").unwrap();
    removed(&holding(vec![data]), &holding(Vec::new()), &namespace).unwrap()
}

fn failed(failure: crate::ObservationFailure, servers: &[&str]) -> VolumeObservation {
    let sought = removing_data()
        .iter()
        .map(|volume| volume.docker_volume.clone())
        .collect();
    let mut observed = VolumeObservation::failed(sought, Vec::new(), failure);
    if let Some(failure) = observed.failure.as_mut() {
        failure.servers = servers.iter().map(|&server| server.to_owned()).collect();
    }
    observed
}

fn refused_connection() -> RpcError {
    RpcError {
        code: ployz_core::RpcErrorCode::Unavailable,
        message: "All 1 connections from the explicit connection failed.".to_owned(),
        details: serde_json::Value::Null,
        cause: vec!["Connection refused (os error 111)".to_owned()],
    }
}

#[test]
fn a_check_that_could_not_reach_its_server_names_it_and_carries_why() {
    let failure = crate::ObservationFailure::unreachable(refused_connection());

    let refused = held(removing_data(), Some(&failed(failure, &["one"]))).unwrap_err();

    assert_eq!(refused.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        refused.message,
        "This removes deployed Volumes, and Ployz couldn't reach Server one to check for their \
         data, so it refuses. Try again once it can be reached"
    );
    assert_eq!(
        refused.details,
        json!({ "volumes": ["data"], "unanswered": ["one"] })
    );
    assert_eq!(
        refused.cause,
        [
            "All 1 connections from the explicit connection failed.",
            "Connection refused (os error 111)",
        ]
    );
}

#[test]
fn a_server_that_answered_the_check_with_an_error_is_named_with_it() {
    let server = MachineId::parse("a".repeat(32)).unwrap();
    let error = RpcError {
        code: ployz_core::RpcErrorCode::Internal,
        message: "The Machine list is unavailable.".to_owned(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    };
    let failure = crate::ObservationFailure::answered(Some(server), error);

    let refused = held(removing_data(), Some(&failed(failure, &["one"]))).unwrap_err();

    assert_eq!(refused.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        refused.message,
        "This removes deployed Volumes, and Server one returned an error when Ployz checked for \
         their data, so it refuses. Fix the error Server one returned, then try again"
    );
    assert_eq!(
        refused.details,
        json!({ "volumes": ["data"], "server": "one" })
    );
    assert_eq!(refused.cause, ["The Machine list is unavailable."]);
}

#[test]
fn a_check_that_never_ran_still_refuses() {
    let refused = held(removing_data(), None).unwrap_err();

    assert_eq!(refused.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        refused.message,
        "This removes deployed Volumes, and the Servers weren't checked for their data, so it \
         refuses. Try again once the Servers can be reached"
    );
    assert_eq!(refused.details, json!({ "volumes": ["data"] }));
}

#[test]
fn servers_that_did_not_answer_are_named_and_one_never_deployed_keeps_its_id() {
    let named: MachineId = "67fc9e737c7045bfbbb96bf9e3c9c6cc".parse().unwrap();
    let unnamed: MachineId = "0a1b2c3d4e5f60718293a4b5c6d7e8f9".parse().unwrap();
    let data = [VolumeName::parse("data").unwrap()];
    let servers = std::collections::BTreeMap::from([(named, "machine-3".to_owned())]);

    let one = silent(&data, &[named], &servers);
    assert_eq!(one.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        one.message,
        "This removes deployed Volumes, and Server machine-3 didn't answer whether it holds \
         their data, so it refuses. Try again once it answers"
    );
    assert_eq!(
        one.details,
        json!({ "volumes": ["data"], "unanswered": [named] })
    );

    let two = silent(&data, &[named, unnamed], &servers);
    assert_eq!(
        two.message,
        format!(
            "This removes deployed Volumes, and Servers machine-3, {unnamed} didn't answer \
             whether they hold their data, so it refuses. Try again once they answer"
        )
    );
}
