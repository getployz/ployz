use serde_json::{Value, json};

use super::*;

const RULE: &str = "0123456789abcdef";
const NONCE: &str = "00c0ffee";

fn machine(byte: char) -> MachineId {
    MachineId::parse(byte.to_string().repeat(32)).unwrap()
}

fn namespace_on(machine: &MachineId) -> Namespace {
    Namespace::parse(format!("pv-{RULE}-{}-{NONCE}", PreviewHome::of(machine))).unwrap()
}

fn physical(namespace: &Namespace) -> DockerVolumeName {
    namespace.volume_name(&DockerVolumeName::parse("db").unwrap())
}

/// A spec mounting tmpfs at `/tmp` and a Config at `/etc/site.conf`, reaching nothing else.
fn contained() -> Value {
    json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": "nginx", "pull_policy": "missing",
            "config_mounts": [{"config_name": "site", "target": "/etc/site.conf"}]},
        "configs": [{"name": "site", "content": [104, 105]}],
        "volumes": [{"reference": "scratch", "source": {"kind": "tmpfs"}}],
        "mounts": [{"volume": "scratch", "target": "/tmp"}],
    })
}

fn spec(value: Value) -> RequestedServiceSpec {
    serde_json::from_value(value).unwrap()
}

/// [`contained`] with `key` of the object at `parent` set to `value`.
fn with(parent: &[&str], key: &str, value: Value) -> RequestedServiceSpec {
    let mut spec_value = contained();
    let object = parent
        .iter()
        .fold(&mut spec_value, |at, step| &mut at[*step]);
    object[key] = value;
    spec(spec_value)
}

/// [`contained`], mounting a Volume of `source` at `/data` too.
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
fn mounting(source: Value) -> RequestedServiceSpec {
    let mut value = contained();
    let mut push = |list: &str, entry: Value| value[list].as_array_mut().unwrap().push(entry);
    push("volumes", json!({"reference": "data", "source": source}));
    push("mounts", json!({"volume": "data", "target": "/data"}));
    spec(value)
}

fn provisioned(name: &DockerVolumeName, birth: Option<&VolumeBirth>) -> RequestedServiceSpec {
    mounting(json!({
        "kind": "provisioned", "name": name, "maximum_bytes": 1_073_741_824u64, "birth": birth,
    }))
}

fn data() -> ContainerPath {
    ContainerPath::parse("/data").unwrap()
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
    let contained = spec(contained());
    assert_eq!(
        admit_preview(&home, &namespace, Admission::Signed, &contained),
        Ok(())
    );
    let elsewhere = Err(PreviewRefusal::Elsewhere {
        namespace: namespace.clone(),
    });
    assert_eq!(
        admit_preview(&other, &namespace, Admission::Signed, &contained),
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
    assert_eq!(
        admit_preview(
            &home,
            &namespace_on(&home),
            Admission::Free,
            &spec(contained())
        ),
        Err(PreviewRefusal::Unsigned)
    );
}

#[test]
fn a_preview_reaches_nothing_of_its_server() {
    let home = machine('a');
    let namespace = namespace_on(&home);
    let host_port = json!([{"mode": "host", "bind": {"kind": "all"}, "published_port": 8080,
        "container_port": 80, "transport_protocol": "tcp"}]);
    let device = json!({"devices": [{"machine_path": "/dev/sda",
        "container_path": "/dev/sda", "cgroup_permissions": "rwm"}]});
    let bind = json!([{"reference": "scratch", "source": {"kind": "tmpfs"}},
        {"reference": "host", "source": {"kind": "bind", "machine_path": "/"}}]);
    for (what, reaches) in [
        (
            "privileged",
            with(&["container"], "privileged", json!(true)),
        ),
        (
            "device /dev/sda:/dev/sda",
            with(&["container"], "resources", device),
        ),
        (
            "reserves nvidia devices",
            with(
                &["container"],
                "resources",
                json!({"device_reservations": [{"driver": "nvidia"}]}),
            ),
        ),
        (
            "adds SYS_ADMIN",
            with(&["container"], "cap_add", json!(["SYS_ADMIN"])),
        ),
        ("pid host", with(&["container"], "pid_mode", json!("host"))),
        (
            "sysctl net.ipv4.ip_forward",
            with(
                &["container"],
                "sysctls",
                json!({"net.ipv4.ip_forward": "1"}),
            ),
        ),
        ("host port 8080", with(&[], "ports", host_port)),
        // Declared, never mounted: the spec is signed whole.
        ("binds /", with(&[], "volumes", bind)),
        (
            "privileged hook",
            with(
                &[],
                "pre_deploy",
                json!({"command": ["migrate"], "privileged": true}),
            ),
        ),
    ] {
        let refusal = admit_preview(&home, &namespace, Admission::Signed, &reaches);
        let Err(PreviewRefusal::HostReach { what: shown }) = refusal else {
            panic!("{what}: {refusal:?}");
        };
        assert!(
            shown.starts_with(what.split(' ').next().unwrap()),
            "{what}: {shown}"
        );
    }
    let network = with(&["container"], "network", json!("tunnel"));
    assert!(matches!(
        admit_preview(&home, &namespace, Admission::Signed, &network),
        Err(PreviewRefusal::HostReach { .. })
    ));
}

#[test]
fn a_preview_mounts_only_its_own_preview_born_volumes() {
    let home = machine('a');
    let namespace = namespace_on(&home);
    let preview = PreviewNamespace::of(&namespace).unwrap();
    let name = physical(&namespace);
    let admit =
        |spec: &RequestedServiceSpec| admit_preview(&home, &namespace, Admission::Signed, spec);
    assert_eq!(
        admit(&provisioned(&name, Some(&preview.birth("db")))),
        Ok(())
    );

    let outside = Err(PreviewRefusal::Outside { target: data() });
    assert_eq!(
        admit(&mounting(json!({ "kind": "bind", "machine_path": "/etc" }))),
        outside
    );
    assert_eq!(
        admit(&mounting(json!({ "kind": "external", "name": name }))),
        outside
    );

    // Another preview's Namespace, with that preview's own Volume and birth.
    let sibling =
        Namespace::parse(format!("pv-{RULE}-{}-0000beef", PreviewHome::of(&home))).unwrap();
    let sibling_name = physical(&sibling);
    let sibling_birth = PreviewNamespace::of(&sibling).unwrap().birth("db");
    let signed_birth = VolumeBirth {
        nonce: crate::BirthNonce::from_bytes([7; 16]),
        ..preview.birth("db")
    };
    let not_its_own = [
        provisioned(&name, None),
        provisioned(&name, Some(&signed_birth)),
        provisioned(&name, Some(&preview.birth("other"))),
        provisioned(&sibling_name, Some(&sibling_birth)),
        provisioned(&sibling_name, Some(&preview.birth("db"))),
        mounting(json!({
            "kind": "ordinary", "name": name, "driver": { "name": "local" },
            "birth": preview.birth("db"),
        })),
    ];
    for spec in not_its_own {
        assert_eq!(
            admit(&spec),
            Err(PreviewRefusal::NotItsOwn { target: data() }),
            "{spec:?}"
        );
    }
}

#[test]
fn a_preview_runs_an_image_only_over_the_volumes_it_declares() {
    let namespace = namespace_on(&machine('a'));
    let spec = spec(contained());
    let image =
        |declared: &[&str]| admit_preview_image(&namespace, &spec, declared.iter().copied());
    assert_eq!(image(&[]), Ok(()));
    assert_eq!(image(&["/tmp", "/tmp/", "/etc/site.conf"]), Ok(()));
    assert_eq!(
        image(&["/tmp", "/var/lib/postgresql/data"]),
        Err(PreviewRefusal::ImageVolume {
            target: "/var/lib/postgresql/data".to_owned()
        })
    );
    let ordinary = Namespace::parse("shop-production").unwrap();
    assert_eq!(
        admit_preview_image(&ordinary, &spec, ["/var/lib/postgresql/data"]),
        Ok(())
    );
}
