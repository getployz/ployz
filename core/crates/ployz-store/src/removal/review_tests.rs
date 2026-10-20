use ployz_core::config::{SavedVolumeIntent, VolumeKind};

use super::*;

fn volume(id: &str, name: &str) -> SavedVolumeIntent {
    SavedVolumeIntent {
        resource_id: id.to_owned(),
        resource_lineage_id: id.to_owned(),
        name: name.to_owned(),
        storage: VolumeKind::Docker {},
        shared_writes: false,
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

#[test]
fn a_check_that_could_not_ask_its_server_refuses_naming_the_server_and_why() {
    let removed = removing_data();
    let sought = removed
        .iter()
        .map(|volume| volume.docker_volume.clone())
        .collect();
    let server = MachineId::parse("a".repeat(32)).unwrap();
    let observed = VolumeObservation::failed(
        sought,
        vec![server],
        "Could not connect to the Machine: connection refused".to_owned(),
    );

    let refused = held(removed, Some(&observed)).unwrap_err();

    assert_eq!(refused.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        refused.message,
        format!(
            "This removes deployed Volumes, and Ployz couldn't check Server {server} for their \
             data, so it refuses: Could not connect to the Machine: connection refused. Try \
             again once it can be reached"
        )
    );
    assert_eq!(
        refused.details,
        json!({
            "volumes": ["data"],
            "unanswered": [server],
            "reason": "Could not connect to the Machine: connection refused",
        })
    );
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
