#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use serde_json::json;

use super::*;
use crate::authority::{
    AuthorityRefusal, Signed,
    deploy::Creation,
    trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
    version::{Budget, VersionFacts, verify},
};
use crate::authority::{
    canonical::from_canonical, keys::Signer, rule::verify_signature, rule_draft::tests::view_json,
};
use crate::{ContainerKind, MachineId, ResolvedUpdateConfig, ServiceId};

const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const CLUSTER: [u8; 32] = [9; 32];

fn spec(name: &str, image: &str) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": name,
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": image, "pull_policy": "missing"},
    }))
    .unwrap()
}

fn asked(pins: BTreeMap<String, String>) -> VersionsAsked {
    VersionsAsked {
        cluster: SealingPublicKey::from_bytes(CLUSTER),
        namespace: Namespace::parse("shop").unwrap(),
        volumes: PlannedVolumes::new(),
        services: vec![
            spec("web", &format!("ghcr.io/acme/web@{DIGEST}")),
            spec("postgres", "postgres:17"),
        ],
        pins,
        operation: OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap(),
        issued_at: Timestamp::from_millis(100_000).unwrap(),
        expires_at: Timestamp::from_millis(700_000).unwrap(),
        seed: [7; 16],
        against: Relayed {
            parents: BTreeMap::new(),
            trusted: Trust::default(),
            containers: Vec::new(),
            unanswered: Vec::new(),
        },
        source: None,
    }
}

fn pinned(pin: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("postgres:17".to_owned(), pin.to_owned())])
}

/// `draft`'s steps riding its bundle, which `passkey` answered.
fn answered(draft: &VersionsDraft, passkey: &Authenticator) -> Vec<Signed<ServiceVersion>> {
    let plan: ProposalApproval = serde_json::from_str(&draft.bundle).unwrap();
    let bundle = Signed::unsigned(&plan).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &bundle.challenge(),
        ..HONEST
    });
    let bundle = bundle.answered(assertion);
    draft
        .steps
        .iter()
        .map(|step| {
            let body: ServiceVersion = serde_json::from_str(&step.body).unwrap();
            Signed::proposed(&body, bundle.clone()).unwrap()
        })
        .collect()
}

fn admit(
    versions: &[Signed<ServiceVersion>],
    passkey: &Authenticator,
    service: &str,
    image: &str,
) -> Result<(), AuthorityRefusal> {
    let mut sent = spec(service, image);
    sent.container.image = image.to_owned();
    let resolved = sent
        .to_resolved(
            ServiceId::parse("c".repeat(32)).unwrap(),
            ResolvedUpdateConfig::default(),
        )
        .unwrap();
    verify(
        versions,
        &VersionFacts {
            cluster: Some(SealingPublicKey::from_bytes(CLUSTER)),
            trusted: &trusting(passkey),
            namespace: &Namespace::parse("shop").unwrap(),
            creation: Creation {
                service: &ServiceName::parse(service).unwrap(),
                kind: ContainerKind::ServiceContainer,
                spec: &resolved,
            },
        },
        &mut Budget::default(),
    )
    .map(drop)
}

#[test]
fn one_passkey_answer_admits_every_service_of_the_draft_with_its_pinned_image() {
    let pin = format!("postgres:17@{DIGEST}");
    let draft = asked(pinned(&pin)).draft().unwrap().unwrap();
    let passkey = Authenticator::es256(4);
    let versions = answered(&draft, &passkey);

    for (service, image) in [
        ("postgres", pin.clone()),
        ("web", format!("ghcr.io/acme/web@{DIGEST}")),
    ] {
        assert_eq!(
            admit(&versions, &passkey, service, &image),
            Ok(()),
            "{service}"
        );
    }
    assert!(
        admit(&versions, &passkey, "postgres", "postgres:17").is_err(),
        "a Server refuses the tag the person never signed"
    );
    assert!(
        draft.lines.iter().any(|line| line.contains(&pin)),
        "the person reads the whole digest Cloud pinned: {:?}",
        draft.lines
    );
}

#[test]
fn a_pin_that_is_not_the_tag_itself_refuses_before_any_bundle() {
    for pin in [
        format!("evil.io/postgres:17@{DIGEST}"),
        format!("postgres:16@{DIGEST}"),
        "postgres:17@sha256:abc".to_owned(),
    ] {
        assert!(
            matches!(
                asked(pinned(&pin)).draft(),
                Err(DraftError::OtherImage(service)) if service.as_str() == "postgres"
            ),
            "{pin}"
        );
    }
    assert!(matches!(
        asked(BTreeMap::new()).draft(),
        Err(DraftError::Unpinned(_))
    ));
}

#[test]
fn a_service_listed_twice_refuses_before_any_bundle() {
    let mut twice = asked(pinned(&format!("postgres:17@{DIGEST}")));
    twice
        .services
        .push(spec("postgres", &format!("postgres:16@{DIGEST}")));
    assert!(matches!(
        twice.draft(),
        Err(DraftError::Twice(service)) if service.as_str() == "postgres"
    ));
}

#[test]
fn data_from_before_births_that_no_server_lists_in_use_refuses_before_any_bundle() {
    let mut legacy = asked(pinned(&format!("postgres:17@{DIGEST}")));
    legacy.services.pop();
    legacy.services.push(serde_json::from_value(json!({
        "name": "postgres",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": "postgres:17", "pull_policy": "missing"},
        "volumes": [{"reference": "data", "source": {"kind": "provisioned", "name": "pg-data", "maximum_bytes": 1024}}],
        "mounts": [{"volume": "data", "target": "/var/lib/postgresql/data"}]
    }))
    .unwrap());
    legacy.against.unanswered = vec!["web-2".to_owned()];
    let refused = legacy.draft().unwrap_err().to_string();
    assert!(
        refused.contains("no server lists a container using it; no answer from web-2"),
        "{refused}"
    );
}

/// A Save's tap: no version, only the Source step drafted from `view_json`'s Git Service.
fn saving(rule: Option<&AutoDeployRule>, sources: serde_json::Value) -> VersionsAsked {
    let view = view_json();
    let mut asked = asked(BTreeMap::new());
    asked.services.clear();
    asked.source = Some(SourceAsked {
        rule: rule.map(|rule| Signed::sign(rule, &Signer::from_seed(&[7; 32])).unwrap()),
        sources: serde_json::from_value(sources).unwrap(),
        target: serde_json::from_value(view["target"].clone()).unwrap(),
        machines: vec![MachineId::parse("1".repeat(32)).unwrap()],
        owner_id: 278_111_222,
        builder: None,
    });
    asked
}

fn first_rule() -> (VersionsDraft, AutoDeployRule) {
    let draft = saving(None, view_json()["sources"].clone())
        .draft()
        .unwrap()
        .unwrap();
    let rule = from_canonical(&draft.source.as_ref().unwrap().body).unwrap();
    (draft, rule)
}

#[test]
fn a_save_signs_the_rule_as_a_source_step_the_servers_accept() {
    let (draft, rule) = first_rule();
    let passkey = Authenticator::es256(4);
    let plan: ProposalApproval = serde_json::from_str(&draft.bundle).unwrap();
    let bundle = Signed::unsigned(&plan).unwrap();
    let bundle = bundle.clone().answered(passkey.answer(&Answer {
        challenge: &bundle.challenge(),
        ..HONEST
    }));
    let step = Signed::proposed(&rule, bundle).unwrap();
    let cluster = SealingPublicKey::from_bytes(CLUSTER);

    let (opened, _) = verify_signature(&step, &trusting(&passkey), Some(cluster)).unwrap();
    assert_eq!(opened.source.branch, "main");
    assert!(draft.steps.is_empty(), "a Save signs no version");
    let summary = &draft.source.unwrap().summary;
    assert_eq!(
        (summary.repository.as_str(), summary.branch.as_str()),
        ("Ployz-test/lab-service", "main"),
        "the person reads the branch the body signs"
    );
}

#[test]
fn a_second_branch_saved_beside_a_kept_rule_leaves_the_rule_alone() {
    let (_, kept) = first_rule();
    let mut sources = view_json()["sources"].clone();
    let mut worker = sources[0].clone();
    worker["service"] = json!("cache");
    worker["branch"] = json!("dev");
    sources.as_array_mut().unwrap().push(worker);

    assert_eq!(
        saving(Some(&kept), sources).draft().unwrap(),
        None,
        "pushes to main keep deploying under the kept rule"
    );
}

#[test]
fn a_changed_setting_signs_the_kept_rules_next_generation() {
    let (_, mut kept) = first_rule();
    kept.services[0].policy.context = "elsewhere".to_owned();

    let draft = saving(Some(&kept), view_json()["sources"].clone())
        .draft()
        .unwrap()
        .unwrap();
    let next: AutoDeployRule = from_canonical(&draft.source.unwrap().body).unwrap();
    assert_eq!((next.rule, next.generation), (kept.rule, 2));
}

#[test]
fn an_ended_rule_stays_ended() {
    let (_, mut kept) = first_rule();
    kept.ended = true;
    kept.services[0].policy.context = "elsewhere".to_owned();

    assert_eq!(
        saving(Some(&kept), view_json()["sources"].clone())
            .draft()
            .unwrap(),
        None
    );
}

#[test]
fn a_builder_other_than_ployzs_release_needs_a_yes() {
    let mut asked = saving(None, view_json()["sources"].clone());
    let fork = Builder {
        r#ref: format!("evil/build/.github/workflows/build.yml@{}", "a".repeat(40)),
        sha: crate::authority::CommitSha::parse(&"a".repeat(40)).unwrap(),
    };
    asked.source.as_mut().unwrap().builder = Some(fork);

    let draft = asked.draft().unwrap().unwrap();
    assert!(
        draft.confirm.iter().any(|line| line.contains("evil/build")),
        "{:?}",
        draft.confirm
    );
}
