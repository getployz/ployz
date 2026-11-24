use std::collections::BTreeMap;

use ployz_core::authority::{ApprovedChange, PayloadDigest};
use ployz_core::{Hint, Namespace, RequestedServiceSpec, ServiceName};
use serde_json::json;

use super::{
    Behind, Enforcing, Needs, Probe, consent, enforced, outdated, pin_tags, signable, unpinnable,
};
use crate::image::PinError;

const SIGNED: Needs = Needs::Person;

const PINNED: &str =
    "nginx@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn spec(image: &str) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": image,
            "pull_policy": "missing",
            "privileged": true,
            "cap_add": ["NET_ADMIN"],
            "resources": {"devices": [{"machine_path": "/dev/fuse", "container_path": "/dev/fuse",
                                       "cgroup_permissions": "rwm"}]},
        },
        "ports": [{"mode": "host", "bind": {"kind": "all"}, "published_port": 8080,
                   "container_port": 80, "transport_protocol": "tcp"}],
        "volumes": [{"reference": "root", "source": {"kind": "bind", "machine_path": "/srv"}}],
        "mounts": [{"volume": "root", "target": "/srv"}],
        "pre_deploy": {"command": ["migrate", "--up"]},
    }))
    .unwrap()
}

fn approved(spec: &RequestedServiceSpec) -> ApprovedChange {
    change(spec, &BTreeMap::new())
}

fn change(spec: &RequestedServiceSpec, pins: &BTreeMap<ServiceName, String>) -> ApprovedChange {
    let [change] = ApprovedChange::of(
        std::slice::from_ref(spec),
        &Namespace::parse("app").unwrap(),
        pins,
        &ployz_core::authority::Mounts {
            namespace: &Namespace::parse("app").unwrap(),
            volumes: &BTreeMap::new(),
            minted: &BTreeMap::new(),
            now: ployz_core::authority::Timestamp::now(),
            against: None,
        },
    )
    .unwrap()
    .try_into()
    .unwrap();
    change
}

/// `spec`'s tag as this CLI pins it: the Servers run `PINNED`, the Saved State says `tag`.
fn pinned(tag: &str) -> ApprovedChange {
    let spec = spec(tag);
    change(
        &spec,
        &BTreeMap::from([(spec.name.clone(), PINNED.to_owned())]),
    )
}

#[test]
fn a_pinned_tag_prints_its_tag_and_the_digest_it_resolved_to() {
    assert_eq!(
        pinned("nginx:1.29-alpine").line(),
        "Approved web: nginx:1.29-alpine → @sha256:0123456789ab… (privileged, device \
         /dev/fuse:/dev/fuse, adds NET_ADMIN, host port 8080, binds /srv, hook migrate --up)"
    );
    let value = serde_json::to_value(pinned("nginx:1.29-alpine")).unwrap();
    assert_eq!(value.get("tag"), Some(&json!("nginx:1.29-alpine")));
    assert_eq!(value.get("image"), Some(&json!(PINNED)));
}

#[test]
fn the_approved_lines_name_the_whole_digest_what_the_host_gives_and_every_setting() {
    assert_eq!(
        approved(&spec(PINNED)).lines(),
        [
            "Approved web: nginx@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
             (privileged, device /dev/fuse:/dev/fuse, adds NET_ADMIN, host port 8080, binds /srv, \
             hook migrate --up)",
            r#"  pre-deploy ["migrate","--up"]"#,
            r#"  mounts [{"no_copy":false,"read_only":false,"target":"/srv","volume":"root"}]"#,
            r#"  ports [{"bind":{"kind":"all"},"container_port":80,"mode":"host","published_port":8080,"transport_protocol":"tcp"}]"#,
            r#"  volumes [{"reference":"root","source":{"create_machine_path":false,"kind":"bind","machine_path":"/srv"}}]"#,
            r#"  cap_add ["NET_ADMIN"]"#,
            r#"  privileged true"#,
            r#"  resources {"device_reservations":[],"devices":[{"cgroup_permissions":"rwm","container_path":"/dev/fuse","machine_path":"/dev/fuse"}],"ulimits":{}}"#,
        ]
    );
}

#[test]
fn json_carries_what_was_approved() {
    let spec = spec(PINNED);
    let value = serde_json::to_value(approved(&spec)).unwrap();
    let digest = PayloadDigest::of(&spec, &Namespace::parse("app").unwrap()).unwrap();
    assert_eq!(
        value,
        json!({
            "service": "web",
            "image": PINNED,
            "digest": digest.to_string(),
            "host": ["privileged", "device /dev/fuse:/dev/fuse", "adds NET_ADMIN", "host port 8080", "binds /srv",
                     "hook migrate --up"],
            "settings": approved(&spec).settings,
        })
    );
}

fn matches() -> clap::ArgMatches {
    crate::handlers::deploy::deploy_command(clap::Command::new("deploy"))
        .try_get_matches_from(["deploy"])
        .unwrap()
}

fn next_hints(error: &crate::handlers::Error) -> Vec<String> {
    error
        .hints()
        .iter()
        .filter_map(|hint| {
            if let Hint::Next(next) = hint {
                Some(next.clone())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn a_private_image_fails_before_admission_asking_for_a_digest() {
    let error = unpinnable(
        &matches(),
        &spec("ghcr.io/acme/web:1.4"),
        PinError::NeedsCredentials {
            registry: "ghcr.io".to_owned(),
            reference: "ghcr.io/acme/web:1.4".to_owned(),
        },
    );
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::InvalidArgument);
    assert_eq!(
        rpc.message,
        "ghcr.io wants credentials to read web's image, so ployz can't pin it. Write its digest."
    );
    assert_eq!(
        next_hints(&error),
        ["ployz set 'web.image=ghcr.io/acme/web@sha256:…'"]
    );
}

#[test]
fn a_registry_that_cannot_be_asked_is_named_with_the_fix() {
    // Nothing listens on this port, so the lookup fails at once, as with no route out.
    let tagged = spec("127.0.0.1:1/acme/web:1.4");
    let error = pin_tags(&matches(), std::slice::from_ref(&tagged)).unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        rpc.message,
        "Couldn't ask 127.0.0.1:1 for web's image digest."
    );
    assert!(!error.causes().is_empty());
    assert_eq!(
        next_hints(&error),
        ["ployz set 'web.image=127.0.0.1:1/acme/web@sha256:…'"]
    );
}

#[test]
fn an_image_written_by_digest_is_not_looked_up() {
    assert!(pin_tags(&matches(), &[spec(PINNED)]).unwrap().is_empty());
}

#[test]
fn a_built_service_is_refused_plainly_with_no_pin_hint() {
    let error = signable(Some(vec![spec("ployz-build/web:pending")])).unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::InvalidArgument);
    assert_eq!(
        rpc.message,
        "web builds from source, and servers that enforce device approval can't run a build \
         yet. Deploy it from an image pinned by digest."
    );
    assert!(next_hints(&error).is_empty(), "{:?}", error.hints());
}

#[test]
fn a_store_that_lowers_no_target_is_refused() {
    let error = signable(None).unwrap_err();
    assert_eq!(error.report().code, ployz_core::RpcErrorCode::Unsupported);
}

#[test]
fn pinned_specs_are_signable() {
    assert_eq!(signable(Some(vec![spec(PINNED)])).unwrap().len(), 1);
}

fn server(seed: u8, enforces: Option<bool>, key: Option<u8>) -> ployz_core::MachineObservation {
    let mut runtime = serde_json::Map::new();
    if let Some(enforces) = enforces {
        runtime.insert("device_enforcement".into(), json!(enforces));
    }
    if let Some(key) = key {
        runtime.insert(
            "sealing".into(),
            json!({"kind": "held", "public_key": base64(&[key; 32])}),
        );
    }
    serde_json::from_value(json!({
        "machine": {
            "id": format!("{seed:02x}").repeat(16),
            "name": format!("machine-{seed}"),
            "subnet": format!("10.210.{seed}.0/24"),
            "public_key": vec![seed; 32],
            "labels": {},
            "accepts_builds": true,
            "accepts_services": true,
            "accepts_ingress": true,
            "advertised_endpoints": [],
            "runtime": runtime,
        },
        "membership": "up",
    }))
    .unwrap()
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn servers_that_do_not_enforce_leave_the_deploy_unsigned() {
    assert_eq!(
        enforced(
            &[server(1, None, Some(9)), server(2, Some(false), Some(9))],
            &BTreeMap::new(),
            SIGNED,
        ),
        Probe::Off
    );
}

#[test]
fn an_enforcing_cluster_with_no_reported_key_is_named() {
    assert_eq!(
        enforced(&[server(1, Some(true), None)], &BTreeMap::new(), SIGNED),
        Probe::NoClusterKey
    );
}

#[test]
fn an_enforcing_cluster_signs_for_every_server_listed() {
    let Probe::Enforcing(Enforcing {
        cluster, machines, ..
    }) = enforced(
        &[server(2, Some(true), None), server(1, Some(false), Some(9))],
        &BTreeMap::new(),
        SIGNED,
    )
    else {
        panic!("expected enforcing");
    };
    assert_eq!(cluster, ployz_core::SealingPublicKey::from_bytes([9; 32]));
    assert_eq!(machines.len(), 2);
    assert!(machines.is_sorted());
}

/// `observation` reporting `capabilities`.
fn serving(
    mut observation: ployz_core::MachineObservation,
    capabilities: &[&str],
) -> ployz_core::MachineObservation {
    observation.machine.runtime.capabilities =
        capabilities.iter().map(|name| (*name).to_owned()).collect();
    observation
}

/// A Server that enforces and serves a list without signed versions refuses every signed
/// Service, so the deploy names it to upgrade before anything is signed. One seen with an
/// empty list, as through a relay, is asked itself: its answer counts, and no answer leaves
/// it unverified, which refuses too. A Server that doesn't enforce takes any creation.
#[test]
fn an_enforcing_server_without_signed_versions_is_named_to_upgrade() {
    let old = ["ployz.container.deploy-approval.v1"];
    let answered = BTreeMap::from([
        (server(5, None, None).machine.id, true),
        (server(6, None, None).machine.id, false),
    ]);
    let Probe::Enforcing(enforcing) = enforced(
        &[
            serving(server(1, Some(true), Some(9)), &old),
            serving(
                server(2, Some(true), Some(9)),
                &[ployz_core::SIGNED_VERSION_CAPABILITY],
            ),
            server(3, Some(true), Some(9)),
            serving(server(4, Some(false), Some(9)), &old),
            server(5, Some(true), Some(9)),
            server(6, Some(true), Some(9)),
        ],
        &answered,
        SIGNED,
    ) else {
        panic!("expected enforcing");
    };
    assert_eq!(
        enforcing.behind,
        Behind {
            outdated: vec!["machine-1".into(), "machine-6".into()],
            unverified: vec!["machine-3".into()],
        }
    );
}

/// The upgrade it names reaches the Cluster this deploy went to.
#[test]
fn the_upgrade_named_keeps_the_cluster_the_deploy_was_given() {
    let typed = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "--connect",
            "ssh://root@entry",
            "deploy",
            "-c",
            "prod",
        ])
        .unwrap();
    let refused = outdated(
        crate::handlers::leaf_matches(&typed),
        &["machine-1".to_owned()],
        ployz_core::SIGNED_VERSION_CAPABILITY,
    );
    assert_eq!(refused.report().code, ployz_core::RpcErrorCode::Unsupported);
    let channel = match ployz_core::MachineVersion::parse(env!("CARGO_PKG_VERSION")) {
        Ok(version) if !version.is_prerelease() => "stable",
        _ => "beta",
    };
    assert_eq!(
        next_hints(&refused),
        [format!(
            "ployz --connect ssh://root@entry server upgrade {channel} machine-1 --context prod"
        )]
    );
}

/// `ployz deploy web`, again.
fn deploying() -> crate::cli::Rerun {
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy", "web"])
        .unwrap();
    crate::cli::Rerun::again(&root)
}

/// Without a terminal nothing is signed: the refusal says to run it at one.
#[test]
fn no_terminal_refuses_before_signing() {
    let approved = [approved(&spec(PINNED))];
    let error = consent(
        (&Namespace::parse("app").unwrap(), &approved),
        deploying(),
        |question, refusal, declined| crate::ui::confirm(question, || refusal, declined),
    )
    .unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::ConfirmationRequired);
    assert!(
        rpc.message.contains("Run it at a terminal"),
        "{}",
        rpc.message
    );
    assert!(
        rpc.message.ends_with("Nothing deployed."),
        "{}",
        rpc.message
    );
}

/// A silent deploy signs an edit only a Server with scoped keys and parent versions
/// admits, so every Server without them is named before anything is signed, enforcing or
/// not, and one that doesn't say whether it enforces. One that doesn't enforce and holds
/// them all passes.
#[test]
fn a_silent_deploy_names_every_server_that_cant_admit_an_edit() {
    let signed = [ployz_core::SIGNED_VERSION_CAPABILITY];
    let all = [
        ployz_core::SIGNED_VERSION_CAPABILITY,
        ployz_core::SCOPED_KEYS_CAPABILITY,
        ployz_core::PARENT_VERSIONS_CAPABILITY,
    ];
    let servers = [
        serving(server(1, Some(true), Some(9)), &signed),
        serving(server(2, Some(true), Some(9)), &all),
        serving(server(3, Some(false), Some(9)), &signed),
        serving(server(4, None, Some(9)), &all),
        serving(server(5, Some(false), Some(9)), &all),
    ];

    let Probe::Enforcing(interactive) = enforced(&servers, &BTreeMap::new(), SIGNED) else {
        panic!("expected enforcing");
    };
    assert_eq!(interactive.behind, Behind::default());
    let Probe::Enforcing(silent) = enforced(&servers, &BTreeMap::new(), Needs::Silent) else {
        panic!("expected enforcing");
    };
    assert_eq!(
        silent.behind,
        Behind {
            outdated: vec!["machine-1".to_owned(), "machine-3".to_owned()],
            unverified: vec!["machine-4".to_owned()],
        }
    );
}

/// A person asked names each Service and the Namespace they sign for, and their no fails
/// the deploy before it signs.
#[test]
fn declining_signs_nothing_and_fails() {
    let approved = [approved(&spec(PINNED))];
    let mut asked = None;
    let error = consent(
        (&Namespace::parse("app").unwrap(), &approved),
        deploying(),
        |question, _, declined| {
            asked = Some((question.to_owned(), declined));
            Err(crate::handlers::Error::cancelled())
        },
    )
    .unwrap_err();
    assert_eq!(
        asked,
        Some((
            "Approve web in namespace app?".to_owned(),
            "Cancelled. Nothing signed or deployed."
        ))
    );
    assert!(error.is_interrupted(), "a no exits 130, not 0");
}

/// A mount that needs a yes is asked about on its own: a person who approves the deploy
/// but says no to the data signs nothing.
#[test]
fn a_no_to_the_marked_data_signs_nothing() {
    let mut change = approved(&spec(PINNED));
    change.confirm = vec!["mounts managed volume app_vol-1 at /srv, read-write: …".to_owned()];
    let error = consent(
        (&Namespace::parse("app").unwrap(), &[change]),
        deploying(),
        |question, _, _| match question {
            "Use the data marked above?" => Err(crate::handlers::Error::cancelled()),
            _ => Ok(()),
        },
    )
    .unwrap_err();
    assert!(error.is_interrupted(), "{}", error.report().message);
}
