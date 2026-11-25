use super::*;
use crate::{ServiceMount, ServiceVolume, ServiceVolumeReference, VolumeOrigin};

const RULE: &str = "0123456789abcdef";
const NONCE: &str = "00c0ffee";

fn machine(byte: char) -> MachineId {
    MachineId::parse(byte.to_string().repeat(32)).unwrap()
}

fn namespace_on(machine: &MachineId) -> Namespace {
    Namespace::parse(format!("pv-{RULE}-{}-{NONCE}", PreviewHome::of(machine))).unwrap()
}

fn physical(namespace: &Namespace) -> DockerVolumeName {
    namespace.volume_name(&DockerVolumeName::parse("vol-db").unwrap())
}

/// A graph mounting one Volume of `source` at `/data`.
fn mounting(source: serde_json::Value) -> ServiceVolumeGraph {
    ServiceVolumeGraph::parse(
        vec![ServiceVolume {
            reference: ServiceVolumeReference::parse("data").unwrap(),
            source: serde_json::from_value(source).unwrap(),
        }],
        vec![ServiceMount {
            volume: ServiceVolumeReference::parse("data").unwrap(),
            target: ContainerPath::parse("/data").unwrap(),
            read_only: false,
            no_copy: false,
            subpath: None,
        }],
    )
    .unwrap()
}

fn provisioned(name: &DockerVolumeName, birth: Option<&VolumeBirth>) -> ServiceVolumeGraph {
    mounting(serde_json::json!({
        "kind": "provisioned", "name": name, "maximum_bytes": 1_073_741_824u64, "birth": birth,
    }))
}

#[test]
fn only_the_exact_form_is_a_preview_namespace() {
    let home = machine('a');
    assert!(PreviewNamespace::of(&namespace_on(&home)).is_some());
    let home = PreviewHome::of(&home);
    for ordinary in [
        "pv-production".to_owned(),
        format!("pv-{RULE}-{home}"),
        format!("pv-{RULE}-{home}-{NONCE}-extra"),
        format!("pv-{}-{home}-{NONCE}", &RULE[..15]),
        format!("pv-{RULE}-{home}-{NONCE}0"),
        format!("pv-{RULE}-{home}-0bad-hex"),
        format!("pr-{RULE}-{home}-{NONCE}"),
    ] {
        let namespace = Namespace::parse(ordinary.as_str()).unwrap();
        assert_eq!(PreviewNamespace::of(&namespace), None, "{ordinary}");
    }
}

#[test]
fn only_the_home_server_admits_a_preview_container_or_its_data() {
    let (home, other) = (machine('a'), machine('b'));
    let namespace = namespace_on(&home);
    let tmpfs = mounting(serde_json::json!({ "kind": "tmpfs" }));
    assert_eq!(
        admit_preview(&home, &namespace, Admission::Signed, &tmpfs),
        Ok(())
    );
    let elsewhere = Err(PreviewRefusal::Elsewhere {
        namespace: namespace.clone(),
    });
    assert_eq!(
        admit_preview(&other, &namespace, Admission::Signed, &tmpfs),
        elsewhere
    );
    assert_eq!(admit_preview_arrival(&home, &physical(&namespace)), Ok(()));
    assert_eq!(
        admit_preview_arrival(&other, &physical(&namespace)),
        elsewhere
    );
    let ordinary = Namespace::parse("shop-production").unwrap();
    assert_eq!(admit_preview_arrival(&other, &physical(&ordinary)), Ok(()));
}

#[test]
fn a_preview_runs_no_free_container() {
    let home = machine('a');
    let tmpfs = mounting(serde_json::json!({ "kind": "tmpfs" }));
    assert_eq!(
        admit_preview(&home, &namespace_on(&home), Admission::Free, &tmpfs),
        Err(PreviewRefusal::Unsigned)
    );
}

#[test]
fn a_preview_mounts_only_its_own_preview_born_volumes() {
    let home = machine('a');
    let namespace = namespace_on(&home);
    let preview = PreviewNamespace::of(&namespace).unwrap();
    let name = physical(&namespace);
    let admit =
        |volumes: &ServiceVolumeGraph| admit_preview(&home, &namespace, Admission::Signed, volumes);
    assert_eq!(
        admit(&provisioned(&name, Some(&preview.birth(&name)))),
        Ok(())
    );

    // Another preview's Namespace, with that preview's own Volume and birth.
    let sibling =
        Namespace::parse(format!("pv-{RULE}-{}-0000beef", PreviewHome::of(&home))).unwrap();
    let sibling_name = physical(&sibling);
    let sibling_birth = PreviewNamespace::of(&sibling).unwrap().birth(&sibling_name);
    let signed_birth = VolumeBirth {
        nonce: crate::BirthNonce::from_bytes([7; 16]),
        ..preview.birth(&name)
    };
    let refused = [
        provisioned(&name, None),
        provisioned(&name, Some(&signed_birth)),
        provisioned(&sibling_name, Some(&sibling_birth)),
        provisioned(&sibling_name, Some(&preview.birth(&sibling_name))),
        mounting(serde_json::json!({ "kind": "bind", "machine_path": "/etc" })),
        mounting(serde_json::json!({ "kind": "external", "name": name })),
        mounting(serde_json::json!({
            "kind": "ordinary", "name": name, "driver": { "name": "local" },
            "birth": preview.birth(&name),
        })),
    ];
    for volumes in refused {
        assert_eq!(
            admit(&volumes),
            Err(PreviewRefusal::Mount {
                target: ContainerPath::parse("/data").unwrap(),
            }),
            "{volumes:?}"
        );
    }
}

#[test]
fn only_a_preview_birth_takes_the_preview_origin() {
    let home = machine('a');
    let namespace = namespace_on(&home);
    let name = physical(&namespace);
    let preview = PreviewNamespace::of(&namespace).unwrap().birth(&name);
    let tagged = |birth: &VolumeBirth| {
        VolumeOrigin::of(birth).unwrap().as_bytes()[..16] == crate::PREVIEW_ORIGIN_TAG[..]
    };
    assert!(tagged(&preview));
    let signed = VolumeBirth {
        nonce: crate::BirthNonce::from_bytes([7; 16]),
        ..preview.clone()
    };
    assert!(!tagged(&signed));
    // The same derivation in an ordinary Namespace marks nothing.
    let ordinary = Namespace::parse("pv-production").unwrap();
    let lookalike = VolumeBirth {
        namespace: ordinary,
        ..preview
    };
    assert!(!tagged(&lookalike));
}
