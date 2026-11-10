//! Previews under a preview rule: two previews of one rule differ only in the fields a
//! Server derives, so the template a Device signs once admits every later preview.

use std::collections::BTreeMap;

use ployz_core::ServiceName;
use ployz_core::authority::preview::{PreviewKey, preview_namespace, template};
use ployz_core::authority::rule::{
    Builder, PolicyCache, RuleEvent, RuleSource, Runner, template as rule_template,
};
use ployz_core::authority::{
    BuildPolicy, CommitSha, PreviewRule, PreviewService, RuleId, Signed, Signer, SignerKey,
    Timestamp,
};
use ployz_core::config::ServiceGitAccess;
use ployz_store::{
    Actor, ApprovePreviewRule, AuthorizedRepository, Change, ClusterDomain, ClusterDomainStatus,
    ConfigStore, CreateGitService, CreateProject, CreateVolume, DomainEvidence, Edit,
    EnvironmentId, EnvironmentName, EnvironmentRef, GithubRule, Mount, OrganizationId,
    PlanPreviewRule, PreviewReviewQuery, PreviewRuleView, ProjectId, ProjectName, PullRequest,
    RuleQuery, ServiceLineageId, SetPrPlan, SettingPath, SystemEvent, Trusted, VolumeId,
    VolumeName, Written,
};
use serde_json::{Value, json};

mod backend;

const HEAD: &str = "1111111111111111111111111111111111111111";
const RULE: [u8; 16] = [0x5a; 16];

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

fn trusted() -> Trusted {
    Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: Vec::new(),
        }],
        domains: DomainEvidence {
            cluster_domain: Some(ClusterDomain {
                name: ployz_store::Hostname::parse("acme.ployz.app").unwrap(),
                status: ClusterDomainStatus::Ready,
            }),
            certificates: Some(Vec::new()),
            ingress_addresses: vec!["203.0.113.7".into()],
            ..DomainEvidence::default()
        },
        servers: Some(1),
        ..backend::members()
    }
}

fn set(store: &ConfigStore, who: &Actor, path: &str, value: Value) {
    store
        .write_trusted(
            who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value,
                }],
            },
            &trusted(),
        )
        .unwrap();
}

/// A production Environment with two Services of `acme/web`, a Volume `web` writes, a
/// Service from an image, a Config, a sealed value, a Generated Value, a generated
/// domain and a pre-deploy command, deployed once, and a PR plan that copies the
/// Volume and seeds it.
fn production(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    for (n, name) in [(3, "web"), (4, "api")] {
        store
            .write_trusted(
                who,
                &CreateGitService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    repository: backend::repo_name("acme/web"),
                    branch: None,
                },
                &trusted(),
            )
            .unwrap();
    }
    store
        .write(
            who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            who,
            &ployz_store::CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("db").unwrap(),
                image: Some("postgres:17".into()),
                template: None,
            },
        )
        .unwrap();
    store
        .write(
            who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(7)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ConfigName::parse("conf").unwrap(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/app".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            who,
            &ployz_store::PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ployz_core::ConfigName::parse("conf").unwrap(),
                file: ployz_core::ConfigFileName::parse("app.yml").unwrap(),
                content: "db: ${{ db.PLOYZ_PRIVATE_DOMAIN }}\nmode: ${{ web.MODE }}".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
    set(
        store,
        who,
        "web.env.DB",
        json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"),
    );
    set(store, who, "web.env.MODE", json!("prod"));
    set(
        store,
        who,
        "web.env.API",
        json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}"),
    );
    set(
        store,
        who,
        "web.env.TOKEN",
        backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
    );
    set(
        store,
        who,
        "web.env.GEN",
        json!("${{ secret(16, \"a-z\") }}"),
    );
    set(store, who, "web.preDeployCommand", json!("migrate"));
    set(store, who, "api.replicas", json!(2));
    store
        .write_trusted(
            who,
            &ployz_store::AddDomain {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                hostname: None,
                prefix: None,
                port: None,
            },
            &trusted(),
        )
        .unwrap();
    let deployment = ployz_store::DeploymentId::parse(uuid(90)).unwrap();
    store
        .write_trusted(
            who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: deployment.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                approval: None,
            }),
            &trusted(),
        )
        .unwrap();
    backend::run(store, &deployment);
    store
        .write(
            who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: Some(vec![ployz_store::NodeName::parse("volumes.data").unwrap()]),
                setup: Some(vec![ployz_store::SetupCommand {
                    service: ServiceName::parse("web").unwrap(),
                    command: "seed".into(),
                }]),
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
}

fn plan(store: &ConfigStore, who: &Actor) -> PreviewRuleView {
    store
        .write_trusted(
            who,
            &PlanPreviewRule {
                environment: at("production"),
                repository_id: 11,
                rule: RuleId::from_bytes(RULE),
            },
            &trusted(),
        )
        .unwrap()
}

/// The preview rule a Device signs over `view`: every Service built from the
/// repository, each with the templates of the rehearsed preview.
fn rule_over(view: &PreviewRuleView, signer: &Signer) -> PreviewRule {
    let mut services: Vec<PreviewService> = view
        .sources
        .iter()
        .map(|source| {
            let spec = view
                .target
                .iter()
                .find(|spec| spec.name == source.service)
                .unwrap();
            let mut service = PreviewService {
                name: source.service.clone(),
                template: ployz_core::authority::PayloadDigest::parse(&"0".repeat(64)).unwrap(),
                setup: None,
                policy: BuildPolicy::derive(
                    &source.root_dir,
                    &source.build,
                    spec,
                    ["linux/amd64".to_owned()].into(),
                    PolicyCache::Off,
                ),
                secrets: None,
                inherit: view
                    .inherited
                    .get(&source.service)
                    .cloned()
                    .unwrap_or_default(),
                generated: view
                    .generated
                    .get(&source.service)
                    .cloned()
                    .unwrap_or_default(),
            };
            service.template = template(spec, &view.sample, &service).unwrap();
            service.setup = view
                .setup
                .iter()
                .find(|setup| setup.name == source.service)
                .map(|setup| template(setup, &view.sample, &service).unwrap());
            service
        })
        .collect();
    services.sort_by(|a, b| a.name.cmp(&b.name));
    services.dedup_by(|a, b| a.name == b.name);
    PreviewRule {
        cluster: ployz_core::SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes(RULE),
        generation: 1,
        machines: Vec::new(),
        parent: view.namespace.clone(),
        parent_id: uuid::Uuid::parse_str(view.environment.id.as_str()).unwrap(),
        domain: view.domain.clone().unwrap(),
        source: RuleSource {
            repository_id: 11,
            owner_id: 1,
            repository: "acme/web".to_owned(),
            branch: "main".to_owned(),
            workflow: ".github/workflows/ployz-build.yml".to_owned(),
            events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
            builder: Builder {
                r#ref: format!("Ployz-test/build/.github/workflows/build.yml@{HEAD}"),
                sha: CommitSha::parse(HEAD).unwrap(),
            },
            runner: Runner::GithubHosted,
        },
        trusted: vec![backend::ada().get()],
        services,
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    }
}

/// Open pull request `number` from `branch`, and return the Deployment it admitted.
fn open_pull_request(
    store: &ConfigStore,
    who: &Actor,
    number: u64,
    branch: &str,
) -> ployz_store::DeploymentId {
    let written = store
        .system(
            &who.organization,
            &SystemEvent::PullRequest(PullRequest {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(number),
                title: "A change".into(),
                author: "ada".into(),
                author_id: Some(backend::ada()),
                bot: false,
                head_branch: backend::git_branch(branch),
                head_repository_id: Some(backend::repo_id(11)),
                head: backend::sha(HEAD),
                target_branch: backend::git_branch("main"),
                commits: 1,
                open: true,
                merge_commit: None,
                target_head: None,
                updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:00:00Z").unwrap(),
            }),
            &trusted(),
        )
        .unwrap();
    let Written::Automated(automated) = &written else {
        panic!("{written:?}");
    };
    automated
        .admitted
        .first()
        .unwrap_or_else(|| panic!("{written:?}"))
        .deployment
        .id
        .clone()
}

/// Each rule Service's template in Environment `environment`, a preview of `branch`.
fn templates(
    store: &ConfigStore,
    who: &Actor,
    rule: &PreviewRule,
    environment: &str,
    branch: &str,
) -> BTreeMap<ServiceName, ployz_core::authority::PayloadDigest> {
    let view = store
        .read_trusted(
            who,
            &RuleQuery {
                environment: at(environment),
            },
            &trusted(),
        )
        .unwrap();
    let derived = preview_namespace(rule.rule, &PreviewKey::Branch(branch.to_owned()));
    assert_eq!(
        view.namespace, derived,
        "{environment} runs in its derived Namespace"
    );
    rule.services
        .iter()
        .map(|service| {
            let spec = view
                .target
                .iter()
                .find(|spec| spec.name == service.name)
                .unwrap();
            (
                service.name.clone(),
                template(spec, &derived, service).unwrap(),
            )
        })
        .collect()
}

#[test]
fn two_previews_of_one_rule_differ_only_in_derived_fields() {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    production(&store, &who);
    let signer = Signer::from_seed(&[3; 32]);
    let view = plan(&store, &who);
    assert_eq!(
        view.sample,
        preview_namespace(
            RuleId::from_bytes(RULE),
            &PreviewKey::Branch("ployz-preview-sample".into())
        )
    );
    let rule = rule_over(&view, &signer);
    let web = rule.service(&ServiceName::parse("web").unwrap()).unwrap();
    assert!(web.inherit.contains("TOKEN"), "{:?}", web.inherit);
    assert!(web.generated.contains_key("GEN"), "{:?}", web.generated);
    assert!(
        web.setup.is_some(),
        "the seed runs on a preview's first Deployment"
    );
    store
        .write_trusted(
            &who,
            &ApprovePreviewRule {
                environment: at("production"),
                rule: Box::new(Signed::sign(&rule, &signer).unwrap()),
            },
            &trusted(),
        )
        .unwrap();
    let opened = open_pull_request(&store, &who, 5, "search");
    open_pull_request(&store, &who, 6, "billing");
    // The preview's GitHub build runs under the rule, on the run its push started.
    let build = |deployment: &ployz_store::DeploymentId| {
        store
            .github_rule(&ployz_store::GithubBuildId {
                deployment: deployment.clone(),
                service: ServiceName::parse("web").unwrap(),
            })
            .unwrap()
    };
    assert!(
        matches!(build(&opened), Some(GithubRule::Preview(signed)) if signed.open().unwrap() == rule),
        "{:?}",
        build(&opened)
    );
    let five = templates(&store, &who, &rule, "pr-5", "search");
    let six = templates(&store, &who, &rule, "pr-6", "billing");
    assert_eq!(five, six, "two previews of one rule share every template");
    for service in &rule.services {
        let digest = *five.get(&service.name).unwrap();
        assert!(
            digest == service.template || Some(digest) == service.setup,
            "{} lowers to what the rule approves",
            service.name
        );
    }
    // The Machine's own check of the automatic deploy rule's template, which keeps the
    // Namespace literal, tells the previews apart.
    let literal = |environment: &str| {
        let view = store
            .read_trusted(
                &who,
                &RuleQuery {
                    environment: at(environment),
                },
                &trusted(),
            )
            .unwrap();
        let spec = view
            .target
            .iter()
            .find(|spec| spec.name.as_str() == "web")
            .unwrap()
            .clone();
        rule_template(&spec, &view.namespace).unwrap()
    };
    assert_ne!(literal("pr-5"), literal("pr-6"));
}

/// Pull request `number` from `branch` by an account the Organization's members leave out.
fn open_untrusted(store: &ConfigStore, who: &Actor, number: u64, branch: &str) -> Written {
    store
        .system(
            &who.organization,
            &SystemEvent::PullRequest(PullRequest {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(number),
                title: "A stranger's change".into(),
                author: "mallory".into(),
                author_id: Some(ployz_store::GithubUserId::parse(99).unwrap()),
                bot: false,
                head_branch: backend::git_branch(branch),
                head_repository_id: Some(backend::repo_id(11)),
                head: backend::sha(HEAD),
                target_branch: backend::git_branch("main"),
                commits: 1,
                open: true,
                merge_commit: None,
                target_head: None,
                updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:00:00Z").unwrap(),
            }),
            &trusted(),
        )
        .unwrap()
}

/// A Device's review of pull request 7's head in `environment`, drafted from its plan as
/// the dashboard drafts one.
fn review_of(
    store: &ConfigStore,
    who: &Actor,
    rule: &PreviewRule,
    signer: &Signer,
    environment: &str,
) -> (EnvironmentId, ployz_core::authority::Review) {
    let view = store
        .read_trusted(
            who,
            &PreviewReviewQuery {
                environment: at(environment),
            },
            &trusted(),
        )
        .unwrap();
    assert_eq!(view.rule.open().unwrap(), *rule);
    assert_eq!(view.key, PreviewKey::Branch("stranger".to_owned()));
    let drafted = ployz_core::authority::PreviewReview::draft(
        rule,
        &view.namespace,
        &view.target,
        ployz_core::authority::ReviewTerms {
            cluster: rule.cluster,
            key: view.key,
            commit: CommitSha::parse(view.commit.as_str()).unwrap(),
            environment: uuid::Uuid::parse_str(view.environment.id.as_str()).unwrap(),
            issued_at: Timestamp::from_millis(1_000).unwrap(),
            expires_at: Timestamp::from_millis(600_000).unwrap(),
        },
    )
    .unwrap();
    let body: ployz_core::authority::PreviewReview =
        ployz_core::authority::canonical::from_canonical(&drafted.body).unwrap();
    (
        view.environment.id.clone(),
        ployz_core::authority::Review::Signed(Signed::sign(&body, signer).unwrap()),
    )
}

#[test]
fn an_untrusted_pull_request_opens_its_preview_and_deploys_only_once_reviewed() {
    let (store, approved, review) = reviewed(false);
    let deployment = approved
        .admitted
        .first()
        .unwrap_or_else(|| panic!("the reviewed head deploys: {approved:?}"))
        .deployment
        .id
        .clone();
    let build = ployz_store::GithubBuildId {
        deployment,
        service: ServiceName::parse("web").unwrap(),
    };
    assert_eq!(store.github_review(&build).unwrap(), Some(review));
    assert!(matches!(
        store.github_rule(&build).unwrap(),
        Some(GithubRule::Preview(_))
    ));
}

#[test]
fn a_review_held_for_ci_rides_with_the_deployment_ci_releases() {
    let (store, approved, review) = reviewed(true);
    assert!(
        approved.admitted.is_empty(),
        "web waits for CI: {approved:?}"
    );
    let Written::Automated(passed) = store
        .system(
            &OrganizationId::parse("org").unwrap(),
            &SystemEvent::CheckSuite(ployz_store::CheckSuite {
                repository_id: backend::repo_id(11),
                suite: 1,
                head: backend::sha(HEAD),
                status: serde_json::from_value(json!("completed")).unwrap(),
                conclusion: Some(serde_json::from_value(json!("success")).unwrap()),
                updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:01:00Z").unwrap(),
            }),
            &trusted(),
        )
        .unwrap()
    else {
        panic!("a check suite's news is automation");
    };
    let deployment = passed
        .admitted
        .first()
        .unwrap_or_else(|| panic!("CI releases the reviewed head: {passed:?}"))
        .deployment
        .id
        .clone();
    let build = ployz_store::GithubBuildId {
        deployment,
        service: ServiceName::parse("web").unwrap(),
    };
    assert_eq!(store.github_review(&build).unwrap(), Some(review));
}

/// An untrusted pull request opened under a preview rule, then approved with a person's
/// review once a stale one is refused; with `ci`, web waits for CI.
fn reviewed(
    ci: bool,
) -> (
    ConfigStore,
    ployz_store::Automated,
    ployz_core::authority::Review,
) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    production(&store, &who);
    if ci {
        set(&store, &who, "web.waitForCi", json!(true));
    }
    let signer = Signer::from_seed(&[3; 32]);
    let rule = rule_over(&plan(&store, &who), &signer);
    store
        .write_trusted(
            &who,
            &ApprovePreviewRule {
                environment: at("production"),
                rule: Box::new(Signed::sign(&rule, &signer).unwrap()),
            },
            &trusted(),
        )
        .unwrap();
    let Written::Automated(opened) = open_untrusted(&store, &who, 7, "stranger") else {
        panic!("a pull request's news is automation");
    };
    assert!(
        opened.admitted.is_empty(),
        "an untrusted head deploys nothing: {opened:?}"
    );
    let (environment, review) = review_of(&store, &who, &rule, &signer, "pr-7");

    let mut other = review.opened().unwrap();
    other.commit = CommitSha::parse(&"2".repeat(40)).unwrap();
    let stale = ployz_core::authority::Review::Signed(Signed::sign(&other, &signer).unwrap());
    let approve = |review: ployz_core::authority::Review| {
        store.write_trusted(
            &who,
            &ployz_store::ApprovePullRequest {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(7),
                head: backend::sha(HEAD),
                reviews: vec![ployz_store::EnvironmentReview {
                    environment: environment.clone(),
                    review,
                }],
            },
            &trusted(),
        )
    };
    let refused = approve(stale).unwrap_err();
    assert!(refused.message.contains("another commit"), "{refused:?}");

    let approved = approve(review.clone()).unwrap();
    (store, approved, review)
}
