use std::collections::BTreeMap;

use ployz_core::authority::{ApprovedChange, PayloadDigest};
use ployz_core::{Hint, Namespace, RequestedServiceSpec, ServiceName};
use serde_json::json;

use super::{
    Behind, Enforcing, Needs, Probe, Ruled, consent, enforced, outdated, pin_tags, signable,
    unpinnable,
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

/// An automatic deploy rule over web, built from main, ended or not.
fn rule_over_web(ended: bool) -> ployz_core::authority::AutoDeployRule {
    use ployz_core::authority::rule::{
        BuildPolicy, Builder, GithubSource, PolicyCache, PolicyMethod, RuleEvent, RuleSource,
        Runner,
    };
    use ployz_core::authority::{AutoDeployRule, RuleId, RuleService, SignerKey, Timestamp};
    AutoDeployRule {
        proposal: None,
        preview: None,
        cluster: ployz_core::SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x11; 16]),
        generation: 1,
        namespace: Namespace::parse("app").unwrap(),
        machines: Vec::new(),
        source: RuleSource::Github(GithubSource {
            repository_id: 1,
            owner_id: 1,
            repository: "acme/web".to_owned(),
            branch: "main".to_owned(),
            workflow: ".github/workflows/ployz-build.yml".to_owned(),
            events: vec![RuleEvent::Push],
            builder: Builder::pinned(),
            runner: Runner::GithubHosted,
        }),
        services: vec![RuleService {
            name: ServiceName::parse("web").unwrap(),
            template: PayloadDigest::parse(&"ab".repeat(32)).unwrap(),
            policy: Some(BuildPolicy {
                method: PolicyMethod::Dockerfile {
                    path: "Dockerfile".to_owned(),
                },
                context: ".".to_owned(),
                target: None,
                args: BTreeMap::new(),
                secrets: Default::default(),
                platforms: ["linux/amd64".to_owned()].into(),
                cache: PolicyCache::Off,
            }),
            secrets: None,
            seeded: None,
        }],
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended,
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes([7; 32])),
    }
}

/// web's source as the review reads it: repository 1, on `branch`.
fn web_on(branch: &str) -> ployz_core::authority::rule_draft::RuleSourceView {
    serde_json::from_value(json!({
        "service": "web", "repository": "acme/web", "repository_id": 1, "branch": branch,
        "root_dir": ".", "build": {"buildMethod": "dockerfile"},
    }))
    .unwrap()
}

/// The kept rule over web, ended or not, with web built from main.
fn kept_over_web(ended: bool) -> Ruled {
    Ruled {
        kept: Some(rule_over_web(ended)),
        sources: vec![web_on("main")],
        ..Ruled::default()
    }
}

/// web built from source beside api, an image by digest.
fn web_and_api() -> Option<Vec<RequestedServiceSpec>> {
    let mut api = spec(PINNED);
    api.name = ServiceName::parse("api").unwrap();
    Some(vec![spec("ployz-build/web:pending"), api])
}

fn refusal(ruled: &Ruled) -> String {
    ruled.building(web_and_api()).unwrap_err().report().message
}

const UNCOVERED: &str = "No automatic deploy rule covers web as it stands, so your servers \
                         won't build it. Turn on Deploy every push, then deploy again.";

#[test]
fn a_built_service_the_kept_rule_approves_builds_under_it_and_only_the_rest_is_signed() {
    let signed = kept_over_web(false)
        .building(web_and_api())
        .unwrap()
        .unwrap();

    let names: Vec<&str> = signed.iter().map(|spec| spec.name.as_str()).collect();
    assert_eq!(names, ["api"]);
}

#[test]
fn a_built_service_the_kept_rule_no_longer_approves_is_refused_before_signing() {
    let changed = Ruled {
        changed: vec![ServiceName::parse("web").unwrap()],
        ..kept_over_web(false)
    };
    let moved = Ruled {
        sources: vec![web_on("release")],
        ..kept_over_web(false)
    };

    for ruled in [changed, moved, Ruled::default()] {
        let error = ruled.building(web_and_api()).unwrap_err();
        assert_eq!(
            error.report().code,
            ployz_core::RpcErrorCode::InvalidArgument
        );
        assert_eq!(error.report().message, UNCOVERED);
    }
}

#[test]
fn a_drafted_rule_that_drops_a_built_service_refuses_it_though_the_kept_rule_covers_it() {
    let mut rule = rule_over_web(false);
    rule.services.clear();
    let summary = ployz_core::authority::RuleSummary {
        namespace: "app".to_owned(),
        source: ployz_core::authority::SourceSummary::Github {
            repository: "acme/web".to_owned(),
            branch: "main".to_owned(),
            workflow: rule.source.github().unwrap().workflow.clone(),
            builder: String::new(),
            pinned: true,
        },
        servers: 0,
        generation: 2,
        ended: false,
        services: Vec::new(),
        preview: None,
    };
    let ruled = Ruled {
        drafted: Some(super::Drafted {
            rule,
            births: Default::default(),
            prior: None,
            summary,
        }),
        ..kept_over_web(false)
    };

    assert_eq!(refusal(&ruled), UNCOVERED);
}

#[test]
fn a_built_service_whose_rule_was_ended_is_refused_until_the_switch_turns_it_on() {
    assert_eq!(
        refusal(&kept_over_web(true)),
        "Automatic deploys are off for web. Turn on Deploy every push to deploy it."
    );
}

#[test]
fn a_rule_that_couldnt_be_read_says_so_rather_than_that_none_covers() {
    let unread = Ruled {
        unread: Some("Couldn't read the automatic deploy rule"),
        ..Ruled::default()
    };

    assert_eq!(
        refusal(&unread),
        "Couldn't read the automatic deploy rule, so web can't build under it. Nothing deployed."
    );
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

/// A Server that says it doesn't enforce runs Ployz 0.3, which refuses every signed
/// Service, so the deploy names it to upgrade before anything is signed, even when it
/// lists every capability.
#[test]
fn a_server_that_does_not_enforce_is_named_to_upgrade() {
    let Probe::Signing(signing) = enforced(
        &[
            server(1, Some(true), Some(9)),
            serving(
                server(2, Some(false), Some(9)),
                &[ployz_core::SIGNED_VERSION_CAPABILITY],
            ),
        ],
        &BTreeMap::new(),
        SIGNED,
    ) else {
        panic!("expected signing");
    };
    assert_eq!(signing.behind.outdated, ["machine-2"]);
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
    let Probe::Signing(Enforcing {
        cluster, machines, ..
    }) = enforced(
        &[server(2, Some(true), None), server(1, Some(false), Some(9))],
        &BTreeMap::new(),
        SIGNED,
    )
    else {
        panic!("expected signing");
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

/// A Server that serves a list without signed versions refuses every signed Service, so the
/// deploy names it to upgrade before anything is signed. One seen with an empty list, as
/// through a relay, is asked itself: its answer counts, and no answer leaves it unverified,
/// which refuses too.
#[test]
fn an_enforcing_server_without_signed_versions_is_named_to_upgrade() {
    let old = ["ployz.container.deploy-approval.v1"];
    let answered = BTreeMap::from([
        (server(5, None, None).machine.id, true),
        (server(6, None, None).machine.id, false),
    ]);
    let Probe::Signing(enforcing) = enforced(
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
        panic!("expected signing");
    };
    assert_eq!(
        enforcing.behind,
        Behind {
            outdated: vec!["machine-1".into(), "machine-4".into(), "machine-6".into()],
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
            "ployz --connect '<CONNECT>' server upgrade {channel} machine-1 --context prod"
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

/// Without a terminal a deploy ships only the images it names, with `--yes`; one that
/// forgets has no such line, since `--yes` never forgets.
#[test]
fn without_a_terminal_a_deploy_ships_only_named_images_and_never_forgets() {
    let line = |args: &[&str]| {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy"].iter().chain(args))
            .unwrap();
        super::without_terminal(
            crate::handlers::leaf_matches(&root),
            crate::cli::Rerun::again(&root),
        )
        .map(|again| again.line())
    };
    assert_eq!(
        line(&["--no-prune"]).as_deref(),
        Some("ployz deploy --no-prune --image '<SERVICE=IMAGE>' --yes")
    );
    assert_eq!(
        line(&["web", "--image", "web=nginx:1"]).as_deref(),
        Some("ployz deploy web --image 'web=nginx:1' --yes")
    );
    assert_eq!(line(&["--no-prune", "--forget-older"]), None);
}

/// Without a terminal nothing is signed: the refusal says to run it at one.
#[test]
fn no_terminal_refuses_before_signing() {
    let approved = [approved(&spec(PINNED))];
    let error = consent(
        (&Namespace::parse("app").unwrap(), &approved, None),
        Some(deploying()),
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
/// admits, so every Server without them is named before anything is signed, and one that
/// doesn't say whether it enforces. One that doesn't enforce is named whatever it holds.
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

    let Probe::Signing(interactive) = enforced(&servers, &BTreeMap::new(), SIGNED) else {
        panic!("expected signing");
    };
    assert_eq!(
        interactive.behind,
        Behind {
            outdated: vec!["machine-3".to_owned(), "machine-5".to_owned()],
            unverified: vec!["machine-4".to_owned()],
        }
    );
    let Probe::Signing(silent) = enforced(&servers, &BTreeMap::new(), Needs::Silent) else {
        panic!("expected signing");
    };
    assert_eq!(
        silent.behind,
        Behind {
            outdated: vec![
                "machine-1".to_owned(),
                "machine-3".to_owned(),
                "machine-5".to_owned(),
            ],
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
        (&Namespace::parse("app").unwrap(), &approved, None),
        Some(deploying()),
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
        (&Namespace::parse("app").unwrap(), &[change], None),
        Some(deploying()),
        |question, _, _| match question {
            "Use the data marked above?" => Err(crate::handlers::Error::cancelled()),
            _ => Ok(()),
        },
    )
    .unwrap_err();
    assert!(error.is_interrupted(), "{}", error.report().message);
}
