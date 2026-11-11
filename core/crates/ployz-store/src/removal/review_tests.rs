use ployz_core::{DockerVolumeId, Namespace};

use super::*;
use crate::deployment::loss::docker_volume;
use crate::id::VolumeId;

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

#[test]
fn the_digest_a_volume_loss_confirmation_binds_keeps_its_text() {
    let who = Actor::system(crate::id::OrganizationId::parse("org").unwrap());
    let environment = EnvironmentId::parse("00000000-0000-4000-8000-000000000002").unwrap();
    let machine: MachineId = "67fc9e737c7045bfbbb96bf9e3c9c6cc".parse().unwrap();
    let data = "00000000-0000-4000-8000-000000000005";
    let removed = RemovedVolume {
        id: VolumeId::parse(data).unwrap(),
        name: VolumeName::parse("data").unwrap(),
        docker_volume: docker_volume(&Namespace::parse("shop-production").unwrap(), data).unwrap(),
    };
    let deletes = vec![DockerVolumeId {
        machine_id: machine,
        name: removed.docker_volume.clone(),
    }];
    let loss = VolumeLoss {
        volume: removed,
        deletes,
    };

    assert_eq!(digest(&who, &environment, &[&loss]), "e729210af70c8760");
}
