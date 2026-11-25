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
        images: Vec::new(),
        on: None,
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
    assert_eq!(opened.source.github().unwrap().branch, "main");
    assert!(draft.steps.is_empty(), "a Save signs no version");
    let summary = &draft.source.unwrap().summary;
    assert_eq!(
        match &summary.source {
            SourceSummary::Github {
                repository, branch, ..
            } => (repository.as_str(), branch.as_str()),
            SourceSummary::Registry { .. } => panic!("a GitHub rule"),
        },
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
    kept.services[0].policy.as_mut().unwrap().context = "elsewhere".to_owned();

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
    kept.services[0].policy.as_mut().unwrap().context = "elsewhere".to_owned();

    assert_eq!(
        saving(Some(&kept), view_json()["sources"].clone())
            .draft()
            .unwrap(),
        None
    );
}

/// Turning Deploy every push on for `branch` of `sources` beside `rule`.
fn turning_on(
    rule: Option<&AutoDeployRule>,
    sources: serde_json::Value,
    branch: &str,
) -> Option<AutoDeployRule> {
    let mut asked = saving(rule, sources);
    asked.source.as_mut().unwrap().on = Some(SourceOn::Branch(PushBranch {
        repository_id: 1_450_985_696,
        branch: branch.to_owned(),
    }));
    let draft = asked.draft().unwrap()?;
    Some(from_canonical(&draft.source.unwrap().body).unwrap())
}

// Catches the switch signing nothing when settings are as the rule was when it ended.
#[test]
fn the_switch_turns_an_ended_rule_on_again_as_its_next_generation() {
    let (_, mut kept) = first_rule();
    kept.ended = true;

    let next = turning_on(Some(&kept), view_json()["sources"].clone(), "main").unwrap();

    assert_eq!(
        (next.rule, next.generation, next.ended),
        (kept.rule, 2, false)
    );
}

// Catches Approve again signing nothing when only a Server or a forgotten version needs
// the rule signed again, which the settings alone don't show.
#[test]
fn approving_again_signs_the_next_generation_even_when_nothing_changed() {
    let (_, kept) = first_rule();

    let next = turning_on(Some(&kept), view_json()["sources"].clone(), "main").unwrap();

    assert_eq!(
        (next.rule, next.generation, next.ended),
        (kept.rule, 2, false)
    );
}

#[test]
fn with_several_branches_the_switch_follows_the_one_the_person_picks() {
    let mut sources = view_json()["sources"].clone();
    let mut cache = sources[0].clone();
    cache["service"] = json!("cache");
    cache["branch"] = json!("dev");
    sources.as_array_mut().unwrap().push(cache);
    assert_eq!(saving(None, sources.clone()).draft().unwrap(), None);

    let rule = turning_on(None, sources, "dev").unwrap();

    assert_eq!(rule.source.github().unwrap().branch, "dev");
    let covered: Vec<&str> = rule
        .services
        .iter()
        .map(|service| service.name.as_str())
        .collect();
    assert_eq!(covered, ["cache"], "web on main no longer deploys on push");
}

/// What a Save of `sources` beside `rule` needs, read from the view alone.
fn need(rule: Option<&AutoDeployRule>, sources: serde_json::Value) -> SourceNeed {
    let asked = saving(rule, sources).source.unwrap();
    source_need(&RuleBasis {
        namespace: Namespace::parse("shop").unwrap(),
        rule: asked.rule,
        sources: asked.sources,
        target: asked.target,
        volumes: PlannedVolumes::new(),
        minted: Births::new(),
        images: Vec::new(),
    })
}

#[test]
fn a_save_knows_from_the_view_alone_whether_its_tap_signs_a_rule() {
    let sources = view_json()["sources"].clone();
    let (_, kept) = first_rule();
    let mut changed = kept.clone();
    changed.services[0].policy.as_mut().unwrap().context = "elsewhere".to_owned();
    let mut ended = changed.clone();
    ended.ended = true;

    for (rule, wanted) in [
        (None, SourceNeed::Rule),
        (Some(&kept), SourceNeed::None),
        (Some(&changed), SourceNeed::Rule),
        (Some(&ended), SourceNeed::None),
    ] {
        assert_eq!(need(rule, sources.clone()), wanted, "{rule:?}");
        let signs = saving(rule, sources.clone()).draft().unwrap().is_some();
        assert_eq!(
            signs,
            wanted == SourceNeed::Rule,
            "the tap agrees: {rule:?}"
        );
    }
}

// Catches a Deploy of another Service asking again for a rule approved for ARM, and the
// yes swapping its builds to amd64.
#[test]
fn a_rule_approved_for_another_platform_keeps_it() {
    let sources = view_json()["sources"].clone();
    let (_, mut kept) = first_rule();
    kept.services[0].policy.as_mut().unwrap().platforms = ["linux/arm64".to_owned()].into();

    assert_eq!(need(Some(&kept), sources.clone()), SourceNeed::None);
    assert_eq!(saving(Some(&kept), sources.clone()).draft().unwrap(), None);
    kept.services[0].policy.as_mut().unwrap().context = "elsewhere".to_owned();
    let draft = saving(Some(&kept), sources).draft().unwrap().unwrap();
    let next: AutoDeployRule = from_canonical(&draft.source.unwrap().body).unwrap();
    assert_eq!(
        next.services[0].policy.as_ref().unwrap().platforms,
        ["linux/arm64".to_owned()].into()
    );
}

#[test]
fn services_that_moved_off_the_kept_branch_onto_two_others_sign_nothing() {
    let (_, kept) = first_rule();
    let mut sources = view_json()["sources"].clone();
    sources[0]["branch"] = json!("dev");
    let mut cache = sources[0].clone();
    cache["service"] = json!("cache");
    cache["branch"] = json!("release");
    sources.as_array_mut().unwrap().push(cache);

    assert_eq!(saving(Some(&kept), sources.clone()).draft().unwrap(), None);
    assert_eq!(need(Some(&kept), sources), SourceNeed::None);
}

#[test]
fn a_service_whose_pushes_dont_deploy_leaves_the_rule_to_the_others() {
    let mut sources = view_json()["sources"].clone();
    let mut cache = sources[0].clone();
    cache["service"] = json!("cache");
    cache["branch"] = json!("dev");
    cache["on_push"] = json!(false);
    sources.as_array_mut().unwrap().push(cache);
    let mut view = view_json();
    view["sources"] = sources.clone();
    let basis: RuleBasis = serde_json::from_value(view).unwrap();

    assert_eq!(
        basis.branch_services(),
        vec![ServiceName::parse("web").unwrap()]
    );
    let draft = saving(None, sources).draft().unwrap().unwrap();
    let rule: AutoDeployRule = from_canonical(&draft.source.unwrap().body).unwrap();
    assert_eq!(rule.source.github().unwrap().branch, "main");
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

fn registry() -> RegistrySource {
    RegistrySource {
        host: "ghcr.io".to_owned(),
        repository: "acme/web".to_owned(),
    }
}

/// A Save beside `rule` with `images` Saved, turning image deploys on for `registry()`
/// when `on`: the rule its tap signs, if any.
fn images_saved(
    rule: Option<&AutoDeployRule>,
    images: serde_json::Value,
    on: bool,
) -> Option<AutoDeployRule> {
    let mut asked = saving(rule, json!([]));
    let source = asked.source.as_mut().unwrap();
    source.images = serde_json::from_value(images).unwrap();
    if on {
        source.on = Some(SourceOn::Images(PushImages {
            registry: registry(),
        }));
    }
    let draft = asked.draft().unwrap()?;
    Some(from_canonical(&draft.source.unwrap().body).unwrap())
}

// Catches the card's switch signing a rule that builds, covers a Service whose image is
// elsewhere, or asks again for a rule that already approves what is Saved.
#[test]
fn turning_image_deploys_on_signs_a_registry_rule_over_that_repositorys_services() {
    let images = json!([
        {"service": "web", "image": format!("ghcr.io/acme/web@{DIGEST}")},
        {"service": "cache", "image": "ghcr.io/acme/cache:1"}
    ]);
    let rule = images_saved(None, images.clone(), true).expect("the switch signs a rule");
    assert_eq!(rule.source, RuleSource::Registry(registry()));
    assert_eq!(rule.generation, 1);
    let services: Vec<_> = rule
        .services
        .iter()
        .map(|service| service.name.as_str())
        .collect();
    assert_eq!(services, ["web"]);
    assert!(
        rule.services
            .iter()
            .all(|service| service.policy.is_none() && service.secrets.is_none())
    );
    assert_eq!(rule.well_formed(), Ok(()));
    // A Save of the same settings signs nothing more.
    assert_eq!(images_saved(Some(&rule), images.clone(), false), None);
    // A changed setting signs the rule's next generation, still for the registry.
    let mut changed = rule.clone();
    changed.services[0].template =
        crate::authority::PayloadDigest::parse(&"cd".repeat(32)).unwrap();
    let next = images_saved(Some(&changed), images.clone(), false).unwrap();
    assert_eq!(
        (next.rule, next.generation, next.source),
        (rule.rule, 2, RuleSource::Registry(registry()))
    );
    // Once off, a Save leaves it off.
    let mut ended = changed;
    ended.ended = true;
    assert_eq!(images_saved(Some(&ended), images, false), None);
    // A second Service on the repository joins the rule.
    let both = json!([
        {"service": "web", "image": format!("ghcr.io/acme/web@{DIGEST}")},
        {"service": "cache", "image": "ghcr.io/acme/web:cache"}
    ]);
    let rule = images_saved(None, both, true).unwrap();
    let services: Vec<_> = rule
        .services
        .iter()
        .map(|service| service.name.as_str())
        .collect();
    assert_eq!(services, ["cache", "web"]);
}
