use std::collections::BTreeMap;

use ployz::volume::{MachineVolume, filter_volumes};
use ployz_core::{
    DockerVolume, DockerVolumeId, DockerVolumeName, MachineId, MachineName, NameMatches,
};

#[test]
fn filtering_and_inspect_keep_equal_names_on_different_machines() {
    let volumes = vec![volume('1', "first", "data"), volume('2', "second", "data")];

    let filtered = filter_volumes(&volumes, &[DockerVolumeName::parse("data").unwrap()]);
    assert_eq!(filtered, volumes);
    assert!(matches!(
        NameMatches::from_matches(filtered),
        NameMatches::Ambiguous { rest, .. } if rest.is_empty()
    ));
    assert_eq!(filter_volumes(&volumes, &[]), volumes);
}

fn volume(machine: char, machine_name: &str, name: &str) -> MachineVolume {
    MachineVolume {
        machine_name: MachineName::parse(machine_name).unwrap(),
        volume: DockerVolume {
            id: DockerVolumeId {
                machine_id: machine_id(machine),
                name: DockerVolumeName::parse(name).unwrap(),
            },
            options: BTreeMap::new(),
            labels: BTreeMap::new(),
            storage: ployz_core::DockerVolumeStorageObservation::Plain {
                driver: "local".into(),
            },
        },
    }
}

fn machine_id(value: char) -> MachineId {
    MachineId::parse(value.to_string().repeat(32)).unwrap()
}
