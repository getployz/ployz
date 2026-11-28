//! The preview rule a person signs from the Store's preview rule view, on SQLite and on
//! Postgres (see `backend`): every Server admits each preview's Containers under it.

use ployz_core::authority::rule::Builder;
use ployz_core::authority::{
    AttestKey, AutoDeployRule, BuildClaim, BuildStage, BuiltImageDigest, CommitAuthority,
    CommitSha, PolicyDigest, PreviewBasis, RuleFacts, RuleId, Signed, Signer, SignerKey, Timestamp,
    deploy::Creation, rule_draft::RuleTerms,
};
use ployz_core::{
    ContainerKind, EnvValue, MachineId, PortPublication, PreviewNamespace, ResolvedUpdateConfig,
    ServiceId, ServiceName,
};
use ployz_store::testing::backend;
use ployz_store::{
    Actor, AddDomain, Admit, AuthorizedRepository, ClusterDomain, ClusterDomainStatus, ConfigStore,
    CreateEnvironment, CreateGitService, CreateProject, CreateService, CreateVolume, Deploy,
    DeployPullRequestPreview, DeploymentId, DomainEvidence, Edit, EnvironmentId, EnvironmentName,
    EnvironmentRef, Hostname, Mount, NotDeployed, OrganizationId, PreviewOutcome, PreviewRuleQuery,
    PreviewsQuery, ProjectId, ProjectName, PullRequest, RunEvidence, RunnerId, SealingRecipient,
    ServiceLineageId, SetPrPlan, SettingPath, Signers, SystemEvent, Trusted, VolumeId, VolumeName,
};

const PINNED: &str =
    "postgres@sha256:0000000000000000000000000000000000000000000000000000000000000001";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

fn machine() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

fn device() -> Signer {
    Signer::from_seed(&[7; 32])
}

fn cluster() -> ployz_core::SealingPublicKey {
    ployz_core::SealingPublicKey::from_bytes([3; 32])
}

/// Cloud's evidence: the repository, a ready Cluster Domain, Server `a` that holds
/// previews and trusts [`device`], and a member's pull requests.
fn cloud() -> Trusted {
    Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ployz_core::config::ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: vec![backend::git_branch("main"), backend::git_branch("staging")],
        }],
        domains: DomainEvidence {
            cluster_domain: Some(ClusterDomain {
                name: Hostname::parse("acme.ployz.app").unwrap(),
                status: ClusterDomainStatus::Ready,
            }),
            certificates: Some(Vec::new()),
            ingress_addresses: vec!["203.0.113.7".into()],
            ..DomainEvidence::default()
        },
        preview_homes: Some(vec![machine()]),
        signers: Some(Signers {
            devices: vec![ployz_core::DeviceKey::from_bytes(
                *device().public().as_bytes(),
            )],
            ..Signers::default()
        }),
        sealing: Some(SealingRecipient {
            public_key: cluster(),
        }),
        ..backend::members()
    }
}

/// Project `shop`: Protected `production`, and `staging`, which previews start from:
/// `web` builds from `acme/web`, holds a Sealed `TOKEN` and a generated domain, and `db`
/// runs a pinned Postgres on Volume `data`. Staging deployed once under the Cluster
/// Domain.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let trusted = cloud();
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
                region: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse(uuid(3)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
                region: None,
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(4)).unwrap(),
                environment: at("staging"),
                name: ServiceName::parse("web").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: Some(backend::git_branch("staging")),
            },
            &trusted,
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(5)).unwrap(),
                environment: at("staging"),
                name: ServiceName::parse("db").unwrap(),
                image: Some(PINNED.into()),
                template: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::provisioned_default(),
                id: VolumeId::parse(uuid(6)).unwrap(),
                environment: at("staging"),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("db").unwrap(),
                    path: "/var/lib/postgresql".into(),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &Edit {
                environment: at("staging"),
                expect: None,
                changes: vec![ployz_store::Change::Set {
                    path: SettingPath::parse("web.env.TOKEN").unwrap(),
                    value: backend::sealed_secret(&uuid(3), &uuid(4), "TOKEN", 1),
                }],
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &AddDomain {
                environment: at("staging"),
                service: ServiceName::parse("web").unwrap(),
                hostname: None,
                prefix: None,
                port: None,
            },
            &trusted,
        )
        .unwrap();
    let id = DeploymentId::parse(uuid(9)).unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at("staging"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &trusted,
        )
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id, &runner).unwrap();
    store
        .record(&id, &runner, RunEvidence::NotExecuted("no Servers".into()))
        .unwrap();
    (store, who)
}

fn terms() -> RuleTerms {
    RuleTerms {
        cluster: cluster(),
        machines: vec![machine()],
        owner_id: 1,
        builder: Builder::pinned(),
        platform: "linux/amd64".to_owned(),
        new_rule: RuleId::from_bytes([9; 16]),
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes(
            *device().public().as_bytes(),
        )),
    }
}

fn opened(number: u64) -> PullRequest {
    PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(number),
        title: "Add search".into(),
        author: "ada".into(),
        author_id: Some(backend::ada()),
        bot: false,
        head_branch: backend::git_branch(&format!("search-{number}")),
        head_repository_id: Some(backend::repo_id(11)),
        head: backend::sha("1111111111111111111111111111111111111111"),
        target_branch: backend::git_branch("main"),
        commits: 1,
        open: true,
        merge_commit: None,
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse("2026-01-01T00:00:00Z").unwrap(),
    }
}

/// A member's ask to deploy pull request `number`'s head as a preview.
fn asked(number: u64) -> DeployPullRequestPreview {
    DeployPullRequestPreview {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(number),
        head: backend::sha("1111111111111111111111111111111111111111"),
    }
}

/// The rule drafted from the preview rule view as Cloud's client reads it, signed and
/// kept in `acme/web`'s plan.
fn sign_previews(store: &ConfigStore, who: &Actor) -> AutoDeployRule {
    let query = PreviewRuleQuery {
        project: None,
        repository: backend::repo_name("acme/web"),
        start_from: EnvironmentName::parse("staging").unwrap(),
    };
    let view = store.read_trusted(who, &query, &cloud()).unwrap();
    let basis: PreviewBasis = serde_json::from_value(serde_json::to_value(&view).unwrap()).unwrap();
    let rule = AutoDeployRule::draft_preview(&basis, terms()).unwrap();
    let set = SetPrPlan {
        project: None,
        repository: backend::repo_name("acme/web"),
        enabled: Some(true),
        start_from: Some(EnvironmentName::parse("staging").unwrap()),
        copy: None,
        setup: None,
        remove_on_close: None,
        include_bots: None,
        preview_rule: Some(Box::new(Signed::sign(&rule, &device()).unwrap())),
        preview_cap: None,
    };
    store.write_trusted(who, &set, &cloud()).unwrap();
    rule
}

/// One signature covers every preview: each pull request's preview Deployment sends specs
/// its Server admits under the rule, the pinned database and the built `web` with the
/// Parent's Sealed `TOKEN` and its own domain alike.
#[test]
fn every_preview_of_a_signed_preview_rule_runs_under_it() {
    let (store, who) = shop();
    let rule = sign_previews(&store, &who);
    let attest = AttestKey::from_bytes([0x55; 32]);
    let built = BuiltImageDigest::from_bytes([0x44; 32]);
    for number in [5, 6] {
        store
            .system(
                &who.organization,
                &SystemEvent::PullRequest(opened(number)),
                &cloud(),
            )
            .unwrap();
        let automated = store.write_trusted(&who, &asked(number), &cloud()).unwrap();
        let admitted = automated
            .admitted
            .first()
            .unwrap_or_else(|| panic!("pull request {number} opened no preview: {automated:?}"));
        let runner = RunnerId::parse("runner").unwrap();
        let claimed = store.claim(&admitted.deployment.id, &runner).unwrap();
        let namespace = &claimed.intent.namespace;
        assert!(PreviewNamespace::of(namespace).is_some(), "{namespace}");
        let kept = claimed.rule.as_ref().map(|rule| rule.open().unwrap());
        assert_eq!(
            kept.as_ref(),
            Some(&rule),
            "the claim carries the preview rule"
        );
        let mut names: Vec<&str> = (claimed.intent.target.iter())
            .map(|spec| spec.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["db", "web"]);
        for spec in &claimed.intent.target {
            let mut spec = spec.clone();
            let pinned = spec.name.as_str() == "db";
            if !pinned {
                spec.container.image = format!("ghcr.io/acme/web@{built}");
                let token = spec.container.environment.get("TOKEN");
                assert!(matches!(token, Some(EnvValue::Sealed(_))), "{token:?}");
                let identity = spec.container.sealed_identity.as_ref().unwrap();
                assert_eq!(identity.environment.to_string(), uuid(3));
                assert!(
                    spec.ports.iter().any(|port| matches!(port,
                        PortPublication::Ingress { hostname, .. }
                            if hostname.as_str().contains(namespace.as_str()))),
                    "{:?}",
                    spec.ports
                );
            }
            spec.mount_graph = spec.mount_graph.scope_to_namespace(namespace).unwrap();
            let spec = spec
                .to_resolved(
                    ServiceId::parse("c".repeat(32)).unwrap(),
                    ResolvedUpdateConfig::default(),
                )
                .unwrap();
            let facts = RuleFacts {
                cluster: cluster(),
                attest,
                machine: &machine(),
                namespace,
                service: &spec.name,
                head: None,
            };
            let creation = Creation {
                service: &spec.name,
                kind: ContainerKind::ServiceContainer,
                spec: &spec,
            };
            let admits = if pinned {
                rule.verify_pinned(&facts, creation)
            } else {
                let claim = BuildClaim {
                    cluster: cluster(),
                    rule: rule.rule,
                    generation: rule.generation,
                    service: spec.name.clone(),
                    policy: PolicyDigest::of(&rule.build_policy(&spec.name).unwrap()).unwrap(),
                    commit: CommitSha::parse("1111111111111111111111111111111111111111").unwrap(),
                    authority: CommitAuthority::Head,
                    stage: BuildStage::Built {
                        image: built,
                        attest,
                    },
                };
                rule.verify_creation(&claim, &facts, creation).map(drop)
            };
            assert_eq!(admits, Ok(()), "{} in {namespace}", spec.name);
        }
    }
}

/// A start-from setting changed since the rule was signed: deploying a preview is refused
/// naming the Service, and the pull request shows why, instead of a deploy every Server
/// refuses.
#[test]
fn a_preview_of_settings_its_rule_does_not_cover_is_not_deployed() {
    let (store, who) = shop();
    sign_previews(&store, &who);
    store
        .write(
            &who,
            &Edit {
                environment: at("staging"),
                expect: None,
                changes: vec![ployz_store::Change::Set {
                    path: SettingPath::parse("web.env.MODE").unwrap(),
                    value: serde_json::json!("debug"),
                }],
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::Command::Publish(ployz_store::Publish {
                id: None,
                environment: at("staging"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
                rule: None,
            }),
        )
        .unwrap();
    store
        .system(
            &who.organization,
            &SystemEvent::PullRequest(opened(5)),
            &cloud(),
        )
        .unwrap();
    let refused = store.write_trusted(&who, &asked(5), &cloud()).unwrap_err();
    assert_eq!(
        refused.code,
        ployz_core::RpcErrorCode::Conflict,
        "{refused:?}"
    );
    assert_eq!(
        refused.details.get("service"),
        Some(&serde_json::json!("web")),
        "{refused:?}"
    );

    let query = PreviewsQuery {
        project: None,
        repository: backend::repo_name("acme/web"),
        number: None,
    };
    let view = store.read_trusted(&who, &query, &cloud()).unwrap();
    let row = view
        .pull_requests
        .iter()
        .find(|row| row.number == backend::pr_number(5));
    assert_eq!(
        row.map(|row| &row.preview),
        Some(&PreviewOutcome::NotDeployed {
            why: Some(NotDeployed::NotCovered {
                service: ServiceName::parse("web").unwrap(),
            }),
        }),
    );
}

/// A member's Deploy pins no commit approval, so the preview's build follows the pull
/// request's own push run, which its Server checks against the rule's pushers, rather
/// than a run Cloud dispatches on a branch the rule never names.
#[test]
fn a_members_preview_builds_on_its_pull_requests_push_run() {
    let (store, who) = shop();
    sign_previews(&store, &who);
    store
        .system(
            &who.organization,
            &SystemEvent::PullRequest(opened(5)),
            &cloud(),
        )
        .unwrap();
    let ada = Actor {
        principal: Some(ployz_store::Principal::parse("ada").unwrap()),
        ..who
    };
    let permitted = Trusted {
        permissions: Some(ployz_store::Permission::ALL.iter().copied().collect()),
        ..cloud()
    };
    let automated = store.write_trusted(&ada, &asked(5), &permitted).unwrap();
    let deployment = &automated.admitted.first().unwrap().deployment.id;
    let web = ployz_store::GithubBuildId::parse(&format!("{deployment}.web")).unwrap();
    assert!(matches!(
        store.github_rule(&web).unwrap(),
        Some(ployz_store::GithubRule::Push(_))
    ));
}
