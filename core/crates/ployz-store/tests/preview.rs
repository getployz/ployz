//! Previews under a PR plan's preview rule, on SQLite and on Postgres (see `backend`): a
//! Parent that can't be production, a deploy on demand refused past the cap, the one
//! Server each preview names, which previews Cloud closes on a Server, and what each
//! pull request's preview shows.

use ployz_core::authority::rule::{Builder, GithubSource, RuleEvent, RuleSource, Runner};
use ployz_core::authority::{AutoDeployRule, PreviewTerms, RuleId, Signed, Signer, SignerKey};
use ployz_core::config::ServiceGitAccess;
use ployz_core::{
    DeployOutcome, DeployPreview, MachineId, Namespace, PreviewNamespace, RpcErrorCode, ServiceName,
};
use ployz_store::testing::backend;
use ployz_store::{
    Actor, Admit, AdmitRemoval, AuthorizedRepository, Automated, BranchHead, ClosePreview,
    ConfigStore, CreateBranch, CreateGitService, CreateProject, DeployPullRequestPreview,
    DeploymentId, EnvironmentId, EnvironmentName, EnvironmentRef, NotDeployed, OpenPreviewsQuery,
    OrganizationId, PrPlansQuery, PreviewOutcome, PreviewsQuery, ProjectId, ProjectName,
    PullRequest, PullRequestQuery, RuleQuery, RunEvidence, RunnerId, SealingRecipient,
    ServiceLineageId, SetEnvironmentProtected, SetPrPlan, ShutDown, Signers, Sweep, SystemEvent,
    Trusted, Written,
};

const HEAD: &str = "1111111111111111111111111111111111111111";

fn uuid(n: u8) -> String {
    // `staging`, the Parent previews start from, sorts after every Environment a test
    // makes, so a write locking in ID order takes it last.
    if n == 3 {
        return "ffffffff-ffff-ffff-ffff-ffffffffffff".into();
    }
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

fn machine(byte: char) -> MachineId {
    MachineId::parse(byte.to_string().repeat(32)).unwrap()
}

/// Project `shop`: Protected `production` runs `web` from `acme/web`'s `main`, root
/// `staging` runs `web` and `api` from its `staging` branch, and root `stage2` runs `web`
/// from it too, each published.
fn shop() -> (ConfigStore, Actor) {
    shop_in(backend::open())
}

/// [`shop`] in `store`.
fn shop_in(store: ConfigStore) -> (ConfigStore, Actor) {
    let who = Actor::system(OrganizationId::parse("org").unwrap());
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
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse(uuid(3)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
                region: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse(uuid(77)).unwrap(),
                project: None,
                name: EnvironmentName::parse("stage2").unwrap(),
                region: None,
            },
        )
        .unwrap();
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: backend::git_branch("main"),
            branches: vec![backend::git_branch("main"), backend::git_branch("staging")],
        }],
        ..Trusted::default()
    };
    let services = [
        (4, "production", "web", None),
        (5, "staging", "web", Some("staging")),
        (6, "staging", "api", Some("staging")),
        (78, "stage2", "web", Some("staging")),
    ];
    for (n, environment, name, branch) in services {
        store
            .write_trusted(
                &who,
                &CreateGitService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: at(environment),
                    name: ServiceName::parse(name).unwrap(),
                    repository: backend::repo_name("acme/web"),
                    branch: branch.map(backend::git_branch),
                },
                &evidence,
            )
            .unwrap();
    }
    for environment in ["production", "staging", "stage2"] {
        store
            .write(
                &who,
                &ployz_store::Command::Publish(ployz_store::Publish {
                    id: None,
                    environment: at(environment),
                    version: None,
                    message: None,
                    accept_volume_loss: Vec::new(),
                    rule: None,
                }),
            )
            .unwrap();
    }
    (store, who)
}

fn namespace_of(store: &ConfigStore, who: &Actor, environment: &str) -> Namespace {
    let query = RuleQuery {
        environment: at(environment),
        staged: false,
    };
    store.read(who, &query).unwrap().namespace
}

/// The Device that signs preview rules.
fn device() -> Signer {
    Signer::from_seed(&[7; 32])
}

/// The Cluster Cloud recorded for the Organization.
fn cluster() -> ployz_core::SealingPublicKey {
    ployz_core::SealingPublicKey::from_bytes([3; 32])
}

/// Cloud's evidence that the Servers of [`cluster`] trust `signer`.
fn trusting_key(signer: &Signer) -> Trusted {
    Trusted {
        signers: Some(Signers {
            devices: vec![ployz_core::DeviceKey::from_bytes(
                *signer.public().as_bytes(),
            )],
            ..Signers::default()
        }),
        sealing: Some(SealingRecipient {
            public_key: cluster(),
        }),
        ..Trusted::default()
    }
}

/// Cloud's evidence that the Servers trust [`device`].
fn trusting() -> Trusted {
    trusting_key(&device())
}

/// Save `set`, with Cloud's evidence of the keys the Servers trust.
fn save(store: &ConfigStore, who: &Actor, set: &SetPrPlan) -> Result<(), ployz_core::RpcError> {
    store.write_trusted(who, set, &trusting()).map(drop)
}

/// A preview rule of `acme/web` whose Parent is `parent`, signed by a Device.
fn rule(store: &ConfigStore, who: &Actor, parent: (u8, &str)) -> Box<Signed<AutoDeployRule>> {
    edited(store, who, parent, |_| {})
}

/// [`rule`], with `edit` made before it is signed.
fn edited(
    store: &ConfigStore,
    who: &Actor,
    parent: (u8, &str),
    edit: impl FnOnce(&mut AutoDeployRule),
) -> Box<Signed<AutoDeployRule>> {
    let signer = device();
    let mut rule = AutoDeployRule {
        proposal: None,
        preview: Some(PreviewTerms {
            parent: uuid(parent.0).parse().unwrap(),
            pushers: Vec::new(),
            pinned: Vec::new(),
        }),
        cluster: cluster(),
        rule: RuleId::from_bytes([9; 16]),
        generation: 1,
        namespace: namespace_of(store, who, parent.1),
        machines: Vec::new(),
        source: RuleSource::Github(GithubSource {
            repository_id: 11,
            owner_id: 1,
            repository: "acme/web".into(),
            branch: String::new(),
            workflow: ".github/workflows/ployz-build.yml".into(),
            events: vec![RuleEvent::Push],
            builder: Builder::pinned(),
            runner: Runner::GithubHosted,
        }),
        services: Vec::new(),
        issued_at: ployz_core::authority::Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes(
            *signer.public().as_bytes(),
        )),
    };
    // The settings it approves, as Cloud's client drafts them from the preview rule view,
    // for a Parent previews can start from.
    if let Some(drafted) = drafted(store, who, parent.1) {
        rule.services = drafted.services;
        rule.preview = drafted.preview.map(|terms| PreviewTerms {
            parent: uuid(parent.0).parse().unwrap(),
            ..terms
        });
    }
    edit(&mut rule);
    Box::new(Signed::sign(&rule, &signer).unwrap())
}

/// The preview rule drafted from `parent`'s preview rule view, unless previews can't
/// start from it.
fn drafted(store: &ConfigStore, who: &Actor, parent: &str) -> Option<AutoDeployRule> {
    let query = ployz_store::PreviewRuleQuery {
        project: None,
        repository: backend::repo_name("acme/web"),
        start_from: EnvironmentName::parse(parent).unwrap(),
    };
    let view = store.read_trusted(who, &query, &Trusted::default()).ok()?;
    let basis: ployz_core::authority::PreviewBasis =
        serde_json::from_value(serde_json::to_value(&view).unwrap()).unwrap();
    let terms = ployz_core::authority::rule_draft::RuleTerms {
        cluster: cluster(),
        machines: Vec::new(),
        owner_id: 1,
        builder: Builder::pinned(),
        platform: "linux/amd64".to_owned(),
        new_rule: RuleId::from_bytes([9; 16]),
        issued_at: ployz_core::authority::Timestamp::from_millis(1_800_000_000_000).unwrap(),
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes(
            *device().public().as_bytes(),
        )),
    };
    Some(AutoDeployRule::draft_preview(&basis, terms).unwrap())
}

fn plan(from: &str) -> SetPrPlan {
    SetPrPlan {
        project: None,
        repository: backend::repo_name("acme/web"),
        enabled: Some(true),
        start_from: Some(EnvironmentName::parse(from).unwrap()),
        copy: None,
        setup: None,
        remove_on_close: None,
        include_bots: None,
        preview_rule: None,
        preview_cap: None,
    }
}

/// Turn previews of `staging` on, with at most `cap` open. The plan keeps a closed pull
/// request's Environment, which a preview never does.
fn previews(store: &ConfigStore, who: &Actor, cap: u8) {
    let set = SetPrPlan {
        preview_rule: Some(rule(store, who, (3, "staging"))),
        preview_cap: std::num::NonZeroU8::new(cap),
        remove_on_close: Some(false),
        ..plan("staging")
    };
    save(store, who, &set).unwrap();
}

fn facts(number: u64, open: bool, target: &str, updated: &str) -> PullRequest {
    PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(number),
        title: "Add search".into(),
        author: "ada".into(),
        author_id: Some(backend::ada()),
        bot: false,
        head_branch: backend::git_branch(&format!("search-{number}")),
        head_repository_id: Some(backend::repo_id(11)),
        head: backend::sha(HEAD),
        target_branch: backend::git_branch(target),
        commits: 1,
        open,
        merge_commit: None,
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse(updated).unwrap(),
    }
}

fn homes(homes: &[char]) -> Trusted {
    Trusted {
        preview_homes: Some(homes.iter().copied().map(machine).collect()),
        ..backend::members()
    }
}

fn observe(store: &ConfigStore, who: &Actor, event: SystemEvent, trusted: &Trusted) -> Automated {
    let Written::Automated(automated) = store.system(&who.organization, &event, trusted).unwrap()
    else {
        panic!("a system event writes Automated")
    };
    automated
}

fn pull(store: &ConfigStore, who: &Actor, event: PullRequest, trusted: &Trusted) -> Automated {
    observe(store, who, SystemEvent::PullRequest(event), trusted)
}

/// Deploy pull request `number`'s head [`HEAD`] as a preview, as a member asks.
fn deploy(
    store: &ConfigStore,
    who: &Actor,
    number: u64,
    trusted: &Trusted,
) -> Result<Automated, ployz_core::RpcError> {
    let asked = DeployPullRequestPreview {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(number),
        head: backend::sha(HEAD),
    };
    store.write_trusted(who, &asked, trusted)
}

/// News of pull request `number` into `target`, then a member's deploy of it.
fn opened(
    store: &ConfigStore,
    who: &Actor,
    (number, target): (u64, &str),
    trusted: &Trusted,
) -> Automated {
    pull(
        store,
        who,
        facts(number, true, target, "2026-01-01T00:00:00Z"),
        trusted,
    );
    deploy(store, who, number, trusted).unwrap()
}

/// The preview pull request `number` has now, if any.
fn preview_of(store: &ConfigStore, who: &Actor, number: u64) -> Option<PreviewNamespace> {
    let name = format!("pr-{number}");
    let listed = store
        .read(who, &ployz_store::EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .any(|listing| listing.name.as_str() == name);
    listed
        .then(|| PreviewNamespace::of(&namespace_of(store, who, &name)))
        .flatten()
}

/// Whether pull request `number` has an open preview, under any of the names its PR
/// Environments took: one closing keeps its name until it is gone.
fn previewing(store: &ConfigStore, who: &Actor, number: u64) -> bool {
    let name = format!("pr-{number}");
    let namespaces = store
        .read(who, &ployz_store::EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .filter(|listing| {
            let listed = listing.name.as_str();
            listed == name || listed.starts_with(&format!("{name}-"))
        })
        .map(|listing| namespace_of(store, who, listing.name.as_str()))
        .collect();
    let asked = OpenPreviewsQuery { namespaces };
    !store.read(who, &asked).unwrap().open.is_empty()
}

fn skipped(automated: &Automated) -> Vec<&str> {
    automated
        .skipped
        .iter()
        .map(|skipped| skipped.reason.as_str())
        .collect()
}

#[test]
fn a_preview_rule_is_kept_only_for_a_parent_that_cannot_be_production() {
    let (store, who) = shop();
    let refused = |set: SetPrPlan| {
        let error = save(&store, &who, &set).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        error.message
    };
    let unprotect = |environment: &str, protected: bool| {
        store
            .write(
                &who,
                &SetEnvironmentProtected {
                    environment: at(environment),
                    protected,
                    seen: None,
                },
            )
            .unwrap();
    };

    // Production as the Parent: it is Protected.
    let production = rule(&store, &who, (2, "production"));
    let message = refused(SetPrPlan {
        preview_rule: Some(production),
        ..plan("production")
    });
    assert!(message.contains("Protected"), "{message}");

    // A Project that never said which Environment is production.
    unprotect("production", false);
    let message = refused(SetPrPlan {
        preview_rule: Some(rule(&store, &who, (3, "staging"))),
        ..plan("staging")
    });
    assert!(message.contains("Protect production first"), "{message}");
    unprotect("production", true);

    // A Branch as the Parent.
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(7)).unwrap(),
                from: at("staging"),
                name: EnvironmentName::parse("qa").unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: true,
                fix: None,
            },
        )
        .unwrap();
    let message = refused(SetPrPlan {
        preview_rule: Some(rule(&store, &who, (7, "qa"))),
        ..plan("qa")
    });
    assert!(message.contains("isn't a Branch"), "{message}");

    // A rule naming another Parent than the plan starts from.
    let message = refused(SetPrPlan {
        preview_rule: Some(rule(&store, &who, (3, "staging"))),
        ..plan("qa")
    });
    assert!(message.contains("sign it again"), "{message}");

    // A rule of another repository, and one naming another Namespace than its Parent's.
    let elsewhere = edited(&store, &who, (3, "staging"), |rule| {
        if let RuleSource::Github(github) = &mut rule.source {
            github.repository_id = 12;
        }
    });
    let message = refused(SetPrPlan {
        preview_rule: Some(elsewhere),
        ..plan("staging")
    });
    assert!(message.contains("another repository"), "{message}");
    let production = namespace_of(&store, &who, "production");
    let misnamed = edited(&store, &who, (3, "staging"), |rule| {
        rule.namespace = production
    });
    let message = refused(SetPrPlan {
        preview_rule: Some(misnamed),
        ..plan("staging")
    });
    assert!(message.contains("another Namespace"), "{message}");

    previews(&store, &who, 5);
    let view = store.read(&who, &PrPlansQuery::default()).unwrap();
    let kept = view
        .plans
        .first()
        .and_then(|plan| plan.preview.as_ref())
        .unwrap();
    assert_eq!((kept.generation, kept.cap.get()), (1, 5));
}

#[test]
fn a_pull_request_into_its_parent_gets_no_preview() {
    let (store, who) = shop();
    previews(&store, &who, 5);
    let trusted = homes(&['a']);
    let into_parent = facts(5, true, "staging", "2026-01-01T00:00:00Z");
    pull(&store, &who, into_parent, &trusted);
    let refused = deploy(&store, &who, 5, &trusted).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        refused.message,
        "This pull request merges into the Environment previews start from, so it gets no preview"
    );
    assert_eq!(preview_of(&store, &who, 5), None);
}

#[test]
fn nothing_deploys_until_a_member_asks_and_a_deploy_past_the_cap_is_refused() {
    let (store, who) = shop();
    previews(&store, &who, 2);
    let trusted = homes(&['a']);
    for number in [5, 6, 7] {
        let news = pull(
            &store,
            &who,
            facts(number, true, "main", "2026-01-01T00:00:00Z"),
            &trusted,
        );
        assert!(news.admitted.is_empty(), "{news:?}");
    }
    let open = |numbers: [u64; 3]| numbers.map(|number| preview_of(&store, &who, number).is_some());
    assert_eq!(open([5, 6, 7]), [false, false, false]);

    for number in [5, 6] {
        let deployed = deploy(&store, &who, number, &trusted).unwrap();
        assert_eq!(deployed.admitted.len(), 1, "{deployed:?}");
    }
    let first = preview_of(&store, &who, 5).unwrap();
    assert!(first.homed_on(&machine('a')));
    assert_eq!(
        first.namespace().as_str().get(..19),
        Some("pv-0909090909090909")
    );
    let again = deploy(&store, &who, 5, &trusted).unwrap();
    assert!(again.admitted.is_empty(), "a live preview opens once");

    let full = deploy(&store, &who, 7, &trusted).unwrap_err();
    assert_eq!(full.code, RpcErrorCode::Conflict, "{full:?}");
    assert_eq!(full.message, "Not deployed: 2 of 2 previews are live");
    assert_eq!(full.details, serde_json::json!({ "live": 2, "limit": 2 }));

    let closed = pull(
        &store,
        &who,
        facts(5, false, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );
    assert!(closed.admitted.is_empty(), "a freed place stays free");
    assert_eq!(open([5, 6, 7]), [false, true, false]);
    let deployed = deploy(&store, &who, 7, &trusted).unwrap();
    assert_eq!(deployed.admitted.len(), 1, "{deployed:?}");
}

#[test]
fn a_preview_opens_on_the_emptiest_server_and_again_elsewhere_once_its_server_leaves() {
    let (store, who) = shop();
    previews(&store, &who, 5);
    let both = homes(&['a', 'b']);
    opened(&store, &who, (5, "main"), &both);
    opened(&store, &who, (6, "main"), &both);
    let first = preview_of(&store, &who, 5).unwrap();
    assert!(first.homed_on(&machine('a')));
    assert!(preview_of(&store, &who, 6).unwrap().homed_on(&machine('b')));

    // Cloud couldn't tell which Servers hold previews: nothing moves.
    let unknown = backend::members();
    let quiet = observe(&store, &who, SystemEvent::Sweep(Sweep { now: 0 }), &unknown);
    assert!(quiet.closing.is_empty());

    // Server a left: its preview closes and opens on b with a new Namespace.
    let left = observe(
        &store,
        &who,
        SystemEvent::Sweep(Sweep { now: 0 }),
        &homes(&['b']),
    );
    assert_eq!(left.admitted.len(), 1);
    let named = |prefix: &str| {
        store
            .read(&who, &ployz_store::EnvironmentsQuery::default())
            .unwrap()
            .environments
            .into_iter()
            .filter(|listing| listing.name.as_str().starts_with(prefix))
            .map(|listing| listing.name.to_string())
            .collect::<Vec<_>>()
    };
    let reopened = named("pr-5").into_iter().last().unwrap();
    let again = PreviewNamespace::of(&namespace_of(&store, &who, &reopened)).unwrap();
    assert!(again.homed_on(&machine('b')));
    assert_ne!(again, first);
    let open = store
        .read(
            &who,
            &OpenPreviewsQuery {
                namespaces: vec![first.namespace().clone()],
            },
        )
        .unwrap();
    assert!(open.open.is_empty(), "the preview on a is closed");
}

#[test]
fn cloud_learns_which_of_a_servers_previews_are_still_open() {
    let (store, who) = shop();
    let nothing = OpenPreviewsQuery::default();
    assert!(
        !store.read(&who, &nothing).unwrap().active,
        "no plan keeps previews"
    );
    previews(&store, &who, 5);
    assert!(
        store.read(&who, &nothing).unwrap().active,
        "a plan keeps previews"
    );
    let trusted = homes(&['a']);
    opened(&store, &who, (5, "main"), &trusted);
    opened(&store, &who, (6, "main"), &trusted);
    let open = preview_of(&store, &who, 5).unwrap().namespace().clone();
    let closed = preview_of(&store, &who, 6).unwrap().namespace().clone();
    pull(
        &store,
        &who,
        facts(6, false, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );
    let unknown = PreviewNamespace::mint(&RuleId::from_bytes([1; 16]), &machine('a'), [2; 4]);

    let asked = OpenPreviewsQuery {
        namespaces: vec![open.clone(), closed, unknown.namespace().clone()],
    };
    assert_eq!(store.read(&who, &asked).unwrap().open, [open]);
}

#[test]
fn an_ordinary_environment_never_takes_a_preview_namespace() {
    let (store, who) = shop();
    let lookalike = "0909090909090909-01234567-89abcdef";
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(8)).unwrap(),
                name: ProjectName::parse("pv").unwrap(),
                default_environment: EnvironmentId::parse(uuid(9)).unwrap(),
                region: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse(uuid(10)).unwrap(),
                project: Some(ProjectName::parse("pv").unwrap()),
                name: EnvironmentName::parse(lookalike).unwrap(),
                region: None,
            },
        )
        .unwrap();
    let query = RuleQuery {
        environment: EnvironmentRef {
            project: Some(ProjectName::parse("pv").unwrap()),
            environment: Some(EnvironmentName::parse(lookalike).unwrap()),
        },
        staged: false,
    };
    let namespace = store.read(&who, &query).unwrap().namespace;
    assert_eq!(PreviewNamespace::of(&namespace), None, "{namespace}");
}

/// Run Deployment `id` to applied, as a runner reports one that ran `web`.
fn run(store: &ConfigStore, id: &DeploymentId) {
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(id, &runner).unwrap();
    let operation = serde_json::json!({"type": "remove_container", "machine_id": "a".repeat(32),
        "container_id": "0".repeat(64)});
    let preview: DeployPreview = serde_json::from_value(serde_json::json!({
        "namespace": claimed.intent.namespace,
        "operations": [{"index": 0, "machine_id": "a".repeat(32), "service_name": "web",
            "operation": operation, "status": {"type": "pending"}}],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(serde_json::json!({"type": "success", "completed": [operation]}))
            .unwrap();
    let executed = RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome),
        removed: Vec::new(),
    };
    store.record(id, &runner, executed).unwrap();
}

/// Admit `admit`, Deployment `id`, as Cloud does, and the Namespace its claim closes.
fn closes(store: &ConfigStore, who: &Actor, id: &DeploymentId, admit: Admit) -> Option<Namespace> {
    store
        .write_trusted(who, &admit, &Trusted::default())
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(id, &runner).unwrap().closes
}

#[test]
fn only_a_preview_s_removal_closes_it_on_its_server() {
    let (store, who) = shop();
    previews(&store, &who, 5);
    let trusted = homes(&['a']);
    for number in [5, 6] {
        let opened = opened(&store, &who, (number, "main"), &trusted);
        run(&store, &opened.admitted.first().unwrap().deployment.id);
    }
    let fresh = || DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();

    let kept = preview_of(&store, &who, 6).unwrap();
    let id = fresh();
    let shut = Admit::ShutDown(ShutDown {
        id: id.clone(),
        environment: at("pr-6"),
        version: None,
        accept_volume_loss: Vec::new(),
    });
    assert_eq!(
        closes(&store, &who, &id, shut),
        None,
        "{} shut down, not closed",
        kept.namespace()
    );

    let closing = preview_of(&store, &who, 5).unwrap();
    pull(
        &store,
        &who,
        facts(5, false, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );
    let id = fresh();
    let removal = Admit::Remove(AdmitRemoval {
        id: id.clone(),
        environment: at("pr-5"),
    });
    assert_eq!(
        closes(&store, &who, &id, removal).as_ref(),
        Some(closing.namespace())
    );
}

#[test]
fn a_preview_rule_is_kept_only_once_a_key_the_servers_trust_signed_it() {
    let (store, who) = shop();
    let set = SetPrPlan {
        preview_rule: Some(rule(&store, &who, (3, "staging"))),
        ..plan("staging")
    };
    let unread = store.write(&who, &set).unwrap_err();
    assert_eq!(unread.code, RpcErrorCode::Unavailable, "{unread:?}");
    let stranger = trusting_key(&Signer::from_seed(&[8; 32]));
    let error = store.write_trusted(&who, &set, &stranger).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
    let elsewhere = SetPrPlan {
        preview_rule: Some(edited(&store, &who, (3, "staging"), |body| {
            body.cluster = ployz_core::SealingPublicKey::from_bytes([4; 32]);
        })),
        ..plan("staging")
    };
    let error = save(&store, &who, &elsewhere).unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::InvalidArgument,
        "another Cluster's rule"
    );
    assert!(
        store
            .read(&who, &PrPlansQuery::default())
            .unwrap()
            .plans
            .iter()
            .all(|plan| plan.preview.is_none())
    );
    save(&store, &who, &set).unwrap();
}

#[test]
fn turning_previews_on_keeps_how_an_open_environment_closes() {
    let (store, who) = shop();
    let keep = SetPrPlan {
        remove_on_close: Some(false),
        ..plan("staging")
    };
    store.write(&who, &keep).unwrap();
    let trusted = homes(&['a']);
    pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
    assert_eq!(preview_of(&store, &who, 5), None, "an ordinary Environment");
    previews(&store, &who, 1);

    let closed = pull(
        &store,
        &who,
        facts(5, false, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );

    assert!(closed.closing.is_empty(), "{:?}", closed.closing);
    let kept = store
        .read(&who, &PrPlansQuery::default())
        .unwrap()
        .plans
        .iter()
        .flat_map(|plan| &plan.open)
        .any(|pr| pr.environment.is_some());
    assert!(kept, "the plan keeps a closed pull request's Environment");
}

#[test]
fn a_sweep_that_cannot_reopen_a_member_s_preview_leaves_it_on_its_server() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    opened(&store, &who, (5, "main"), &homes(&['a']));
    let first = preview_of(&store, &who, 5).unwrap().namespace().clone();
    // A sweep that says which Servers can hold a preview but not who the members are
    // couldn't open the member's preview again elsewhere.
    let unknown = Trusted {
        members: None,
        ..homes(&['b'])
    };

    let swept = observe(&store, &who, SystemEvent::Sweep(Sweep { now: 0 }), &unknown);

    assert!(swept.closing.is_empty(), "{:?}", swept.closing);
    let asked = OpenPreviewsQuery {
        namespaces: vec![first],
    };
    assert_eq!(store.read(&who, &asked).unwrap().open.len(), 1);
}

#[test]
fn a_pull_request_retargeted_into_its_parent_closes_its_preview_and_frees_its_place() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    opened(&store, &who, (5, "main"), &trusted);
    pull(
        &store,
        &who,
        facts(6, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
    let first = preview_of(&store, &who, 5).unwrap().namespace().clone();

    let retargeted = pull(
        &store,
        &who,
        facts(5, true, "staging", "2026-01-02T00:00:00Z"),
        &trusted,
    );

    let asked = OpenPreviewsQuery {
        namespaces: vec![first],
    };
    assert!(store.read(&who, &asked).unwrap().open.is_empty());
    assert!(retargeted.admitted.is_empty(), "{retargeted:?}");
    let deployed = deploy(&store, &who, 6, &trusted).unwrap();
    assert_eq!(deployed.admitted.len(), 1, "{deployed:?}");
}

#[test]
fn a_preview_a_sweep_may_have_closed_never_opens_again() {
    let (store, who) = shop();
    previews(&store, &who, 5);
    let trusted = homes(&['a']);
    let opened = opened(&store, &who, (5, "main"), &trusted);
    run(&store, &opened.admitted.first().unwrap().deployment.id);
    let asked = OpenPreviewsQuery {
        namespaces: vec![preview_of(&store, &who, 5).unwrap().namespace().clone()],
    };
    pull(
        &store,
        &who,
        facts(5, false, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );
    // A sweep reading now closes it on its Server, which never opens it again.
    assert!(store.read(&who, &asked).unwrap().open.is_empty());

    let cancel = ployz_store::CancelRemoval {
        project: ProjectName::parse("shop").unwrap(),
        environment: Some(EnvironmentName::parse("pr-5").unwrap()),
    };
    let refused = store.write(&who, &cancel).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert!(store.read(&who, &asked).unwrap().open.is_empty());

    let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    let removal = Admit::Remove(AdmitRemoval {
        id: id.clone(),
        environment: at("pr-5"),
    });
    store
        .write_trusted(&who, &removal, &Trusted::default())
        .unwrap();
    store
        .write(&who, &ployz_store::Cancel { deployment: id })
        .unwrap();
    assert!(
        store.read(&who, &asked).unwrap().open.is_empty(),
        "cancelling its removal's Deployment leaves it closing"
    );
    let swept = observe(&store, &who, SystemEvent::Sweep(Sweep { now: 0 }), &trusted);
    assert!(
        swept
            .closing
            .iter()
            .any(|environment| environment.name.as_str() == "pr-5"),
        "the sweep removes it again: {:?}",
        swept.closing
    );

    let reopened = pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-03T00:00:00Z"),
        &trusted,
    );
    assert!(reopened.admitted.is_empty(), "{reopened:?}");
    let deployed = deploy(&store, &who, 5, &trusted).unwrap();
    assert_eq!(deployed.admitted.len(), 1, "{deployed:?}");
    assert!(store.read(&who, &asked).unwrap().open.is_empty());
}

#[test]
fn a_close_and_a_retarget_in_one_observation_close_the_preview_once() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let removes = SetPrPlan {
        remove_on_close: Some(true),
        ..plan("staging")
    };
    store.write(&who, &removes).unwrap();
    let trusted = homes(&['a']);
    opened(&store, &who, (5, "main"), &trusted);

    let both = SystemEvent::PullRequest(facts(5, false, "staging", "2026-01-02T00:00:00Z"));
    store
        .system(&who.organization, &both, &trusted)
        .expect("one observation closes it once");

    assert_eq!(preview_of(&store, &who, 5), None);
}

#[test]
fn a_preview_no_server_could_take_deploys_again_once_one_returns() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    opened(&store, &who, (5, "main"), &homes(&['a']));
    let sweep = || SystemEvent::Sweep(Sweep { now: 0 });
    let none = observe(&store, &who, sweep(), &homes(&[]));
    assert!(none.admitted.is_empty(), "no Server can take it");
    assert_eq!(
        skipped(&none),
        ["No Server can hold a preview: upgrade one, or let one take Services"]
    );

    let back = observe(&store, &who, sweep(), &homes(&['b']));
    assert!(back.admitted.is_empty(), "nothing opens on its own");

    let deployed = deploy(&store, &who, 5, &homes(&['b'])).unwrap();
    assert_eq!(deployed.admitted.len(), 1, "{deployed:?}");
}

#[test]
fn a_merge_into_a_branch_nothing_deploys_closes_the_preview_and_frees_its_place() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    opened(&store, &who, (5, "feature"), &trusted);
    pull(
        &store,
        &who,
        facts(6, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
    let asked = OpenPreviewsQuery {
        namespaces: vec![preview_of(&store, &who, 5).unwrap().namespace().clone()],
    };
    let mut merged = facts(5, false, "feature", "2026-01-03T00:00:00Z");
    merged.merge_commit = Some(backend::sha(HEAD));

    let landed = pull(&store, &who, merged, &trusted);

    assert!(store.read(&who, &asked).unwrap().open.is_empty());
    assert!(landed.admitted.is_empty(), "{landed:?}");
    let deployed = deploy(&store, &who, 6, &trusted).unwrap();
    assert_eq!(deployed.admitted.len(), 1, "{deployed:?}");
}

#[test]
fn a_retarget_closes_a_preview_by_the_parent_its_branch_was_made_from() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    opened(&store, &who, (5, "main"), &trusted);
    let first = preview_of(&store, &who, 5).unwrap().namespace().clone();
    // The plan now starts elsewhere; the preview was made from staging.
    store.write(&who, &plan("production")).unwrap();

    pull(
        &store,
        &who,
        facts(5, true, "staging", "2026-01-02T00:00:00Z"),
        &trusted,
    );

    let asked = OpenPreviewsQuery {
        namespaces: vec![first],
    };
    assert!(store.read(&who, &asked).unwrap().open.is_empty());
}

#[test]
fn a_preview_rule_a_device_approved_as_a_bundle_s_source_step_is_kept() {
    use ployz_core::authority::{
        ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind,
    };
    let (store, who) = shop();
    let mut body = rule(&store, &who, (3, "staging")).open().unwrap();
    let operation = ployz_core::OperationId::parse("e".repeat(32)).unwrap();
    body.proposal = Some(ProposalRef {
        operation,
        index: 0,
    });
    let step = ployz_core::authority::canonical::to_canonical(&body).unwrap();
    let bundle = ProposalApproval {
        cluster: body.cluster,
        environment: body.namespace.clone(),
        operation,
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Source,
            digest: StepDigest::of(&step),
        }],
        issued_at: body.issued_at,
        expires_at: ployz_core::authority::Timestamp::from_millis(1_800_000_001_000).unwrap(),
        signer: body.signer,
    };
    let signed = Signed::proposed(&body, Signed::sign(&bundle, &device()).unwrap()).unwrap();
    let set = SetPrPlan {
        preview_rule: Some(Box::new(signed)),
        ..plan("staging")
    };

    save(&store, &who, &set).expect("a Server opens this rule");
}

#[test]
fn a_preview_rule_naming_another_signer_than_the_one_who_signed_it_is_refused() {
    let (store, who) = shop();
    let other = Signer::from_seed(&[8; 32]);
    let set = SetPrPlan {
        preview_rule: Some(edited(&store, &who, (3, "staging"), |body| {
            body.signer = SignerKey::Device(ployz_core::DeviceKey::from_bytes(
                *other.public().as_bytes(),
            ));
        })),
        ..plan("staging")
    };

    let error = save(&store, &who, &set).unwrap_err();

    assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
}

#[test]
fn only_a_member_s_pull_request_from_the_repository_deploys_as_a_preview() {
    let (store, who) = shop();
    previews(&store, &who, 2);
    let trusted = homes(&['a']);
    let mut stranger = facts(5, true, "main", "2026-01-01T00:00:00Z");
    stranger.author = "mallory".into();
    stranger.author_id = Some(ployz_store::GithubUserId::parse(999_999).unwrap());
    pull(&store, &who, stranger, &trusted);
    let mut fork = facts(6, true, "main", "2026-01-01T00:00:00Z");
    fork.head_repository_id = Some(backend::repo_id(12));
    pull(&store, &who, fork, &trusted);

    for (number, author) in [(5, "mallory"), (6, "ada")] {
        let refused = deploy(&store, &who, number, &trusted).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
        assert_eq!(
            refused.message,
            format!("Not deployed: {author} isn't in your organization on Ployz")
        );
    }
    let approve = ployz_store::ApprovePullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        head: backend::sha(HEAD),
    };
    store.write_trusted(&who, &approve, &trusted).unwrap();
    assert_eq!(
        preview_of(&store, &who, 5),
        None,
        "an approval opens no preview"
    );
    assert_eq!(preview_of(&store, &who, 6), None);
}

/// Set `path` to `value` in `environment`.
fn set(store: &ConfigStore, who: &Actor, environment: &str, path: &str, value: &str) {
    let edit = ployz_store::Edit {
        environment: at(environment),
        expect: None,
        changes: vec![ployz_store::Change::Set {
            path: ployz_store::SettingPath::parse(path).unwrap(),
            value: serde_json::Value::String(value.into()),
        }],
    };
    store.write(who, &edit).unwrap();
}

#[test]
fn a_preview_holding_what_its_merge_left_to_land_stays_open_and_gets_no_close() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    opened(&store, &who, (5, "feature"), &trusted);
    let opened = preview_of(&store, &who, 5).unwrap().namespace().clone();
    set(&store, &who, "pr-5", "web.env.A", "2");
    // Nothing deploys `feature`, so the change stays for its Parent to take.
    let mut merged = facts(5, false, "feature", "2026-01-02T00:00:00Z");
    merged.merge_commit = Some(backend::sha(HEAD));

    let first = pull(&store, &who, merged.clone(), &trusted);
    let swept = observe(&store, &who, SystemEvent::Sweep(Sweep { now: 0 }), &trusted);
    merged.updated = ployz_store::GithubTimestamp::parse("2026-01-03T00:00:00Z").unwrap();
    let later = pull(&store, &who, merged, &trusted);

    for automated in [&first, &swept, &later] {
        assert!(
            automated.closing.is_empty() && automated.removed.is_empty(),
            "{automated:?}"
        );
    }
    let asked = OpenPreviewsQuery {
        namespaces: vec![opened],
    };
    assert_eq!(store.read(&who, &asked).unwrap().open.len(), 1);
}

#[test]
fn a_deploy_refused_at_a_full_cap_costs_the_same_however_many_pull_requests_are_open() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let news = |number: u64| {
        let event = facts(number, true, "main", "2026-01-01T00:00:00Z");
        pull(&store, &who, event, &trusted)
    };
    // What a deploy refused at the full cap costs once `waiting` more pull requests are open.
    let cost = |first: u64, waiting: u64| {
        for number in first..=first + waiting {
            news(number);
        }
        let (refused, statements) =
            backend::statements(|| deploy(&store, &who, first + waiting, &trusted));
        assert_eq!(refused.unwrap_err().code, RpcErrorCode::Conflict);
        statements
    };
    opened(&store, &who, (1, "main"), &trusted);

    let few = cost(2, 2);
    let many = cost(10, 40);

    assert_eq!(few, many, "a deploy reads no other pull request");
}

/// Two writes that each close preview `pr-5` as its Server leaves and open it again on
/// another, queued behind a third holding the plan's start-from, both finish and keep
/// the cap:
/// each takes the plan's previews in the same order. Postgres only: SQLite writers
/// never overlap.
#[test]
fn a_sweep_and_news_of_a_pull_request_both_moving_a_preview_both_finish() {
    if std::env::var_os("PLOYZ_STORE_TEST_POSTGRES").is_none() {
        return;
    }
    let url = backend::fresh_url(&tempfile::tempdir().unwrap());
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    previews(&store, &who, 1);
    opened(&store, &who, (5, "main"), &homes(&['a']));
    pull(
        &store,
        &who,
        facts(6, true, "main", "2026-01-01T00:00:00Z"),
        &homes(&['a']),
    );
    let five = preview_of(&store, &who, 5).unwrap().namespace().clone();
    let mut other = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut held = other.transaction().unwrap();
    held.execute(
        "UPDATE config_environment SET working_revision = working_revision WHERE id = $1",
        &[&uuid(3)],
    )
    .unwrap();

    // Until `writes` of this Store's wait for a row lock.
    let queued = |writes: i64| {
        let waiting = "SELECT COUNT(*) FROM pg_stat_activity \
                       WHERE datname = current_database() AND wait_event_type = 'Lock'";
        while backend::count(&url, waiting) < writes {}
    };
    let moved = homes(&['b']);
    let (news, sweep) = std::thread::scope(|threads| {
        let news = threads.spawn(|| {
            let event = SystemEvent::PullRequest(facts(6, true, "main", "2026-01-02T00:00:00Z"));
            store.system(&who.organization, &event, &moved).map(drop)
        });
        queued(1);
        let sweep = threads.spawn(|| {
            let event = SystemEvent::Sweep(Sweep { now: 0 });
            store.system(&who.organization, &event, &moved).map(drop)
        });
        queued(2);
        held.rollback().unwrap();
        (news.join().unwrap(), sweep.join().unwrap())
    });

    assert_eq!((news, sweep), (Ok(()), Ok(())));
    let asked = OpenPreviewsQuery {
        namespaces: vec![five.clone()],
    };
    assert!(store.read(&who, &asked).unwrap().open.is_empty());
    // 5 opens again, on b; 6, which nobody deployed, doesn't.
    let reopened = store
        .read(&who, &ployz_store::EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .filter_map(|listing| {
            PreviewNamespace::of(&namespace_of(&store, &who, listing.name.as_str()))
        })
        .filter(|preview| preview.namespace() != &five)
        .collect::<Vec<_>>();
    assert_eq!(reopened.len(), 1, "{reopened:?}");
    assert!(
        reopened
            .iter()
            .all(|preview| preview.homed_on(&machine('b')))
    );
    assert_eq!(preview_of(&store, &who, 6), None, "the cap holds");
}

/// [`shop`] on Postgres, with its URL; none on SQLite, whose writers never overlap.
fn shop_on_postgres() -> Option<(ConfigStore, Actor, String)> {
    std::env::var_os("PLOYZ_STORE_TEST_POSTGRES")?;
    let url = backend::fresh_url(&tempfile::tempdir().unwrap());
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    Some((store, who, url))
}

/// Until `sessions` of `url`'s database wait for a lock.
fn queued(url: &str, sessions: i64) {
    settled(url, sessions, || false);
}

/// Until `sessions` of `url`'s database wait for a lock, or `done`.
fn settled(url: &str, sessions: i64, done: impl Fn() -> bool) {
    let waiting = "SELECT COUNT(*) FROM pg_stat_activity \
                   WHERE datname = current_database() AND wait_event_type = 'Lock'";
    let start = std::time::Instant::now();
    while !done() && backend::count(url, waiting) < sessions {
        assert!(start.elapsed().as_secs() < 30, "{sessions} never queued");
    }
}

/// Hold `staging`, the Parent, until the returned transaction ends, as a write that
/// reached it first does.
fn hold_parent(client: &mut postgres::Client) -> postgres::Transaction<'_> {
    let mut held = client.transaction().unwrap();
    held.execute(
        "UPDATE config_environment SET working_revision = working_revision WHERE id = $1",
        &[&uuid(3)],
    )
    .unwrap();
    held
}

#[test]
fn a_parent_changed_while_a_preview_opens_lets_no_second_past_the_cap() {
    let Some((store, who, url)) = shop_on_postgres() else {
        return;
    };
    previews(&store, &who, 1);
    let moved = SetPrPlan {
        preview_rule: Some(rule(&store, &who, (77, "stage2"))),
        ..plan("stage2")
    };
    let trusted = homes(&['a']);
    for number in [5, 6] {
        let event = facts(number, true, "feature", "2026-01-01T00:00:00Z");
        pull(&store, &who, event, &trusted);
    }
    let mut gate = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    gate.batch_execute(
        "CREATE FUNCTION pause_pr_5() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.name = 'pr-5' THEN PERFORM pg_advisory_xact_lock(54321); END IF; \
         RETURN NEW; END $$; \
         CREATE TRIGGER pause_pr_5 BEFORE INSERT ON config_environment \
         FOR EACH ROW EXECUTE FUNCTION pause_pr_5(); \
         SELECT pg_advisory_lock(54321)",
    )
    .unwrap();

    let (first, moving, second) = std::thread::scope(|threads| {
        let first = threads.spawn(|| deploy(&store, &who, 5, &trusted));
        queued(&url, 1);
        // Each either queues behind the first or finishes before it.
        let moving = threads.spawn(|| save(&store, &who, &moved));
        settled(&url, 2, || moving.is_finished());
        let second = threads.spawn(|| deploy(&store, &who, 6, &trusted));
        settled(&url, 3, || second.is_finished());
        gate.batch_execute("SELECT pg_advisory_unlock(54321)")
            .unwrap();
        (
            first.join().unwrap(),
            moving.join().unwrap(),
            second.join().unwrap(),
        )
    });

    assert_eq!(first.unwrap().admitted.len(), 1);
    moving.unwrap();
    let refused = second.unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(preview_of(&store, &who, 6), None, "the cap holds");
}

#[test]
fn a_rename_queued_behind_the_deploy_that_opened_its_preview_retracks_it() {
    let Some((store, who, url)) = shop_on_postgres() else {
        return;
    };
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    pull(
        &store,
        &who,
        facts(5, true, "feature", "2026-01-01T00:00:00Z"),
        &trusted,
    );
    let mut renamed = facts(5, true, "feature", "2026-01-03T00:00:00Z");
    renamed.head_branch = backend::git_branch("renamed");
    let mut gate = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let held = hold_parent(&mut gate);
    let news = SystemEvent::PullRequest(renamed);

    let (deployed, news) = std::thread::scope(|threads| {
        let deployed = threads.spawn(|| deploy(&store, &who, 5, &trusted));
        queued(&url, 1);
        let news = threads.spawn(|| store.system(&who.organization, &news, &trusted));
        queued(&url, 2);
        held.rollback().unwrap();
        (deployed.join().unwrap(), news.join().unwrap())
    });

    assert!(deployed.is_ok() && news.is_ok(), "{deployed:?} {news:?}");
    assert!(preview_of(&store, &who, 5).is_some());
    let tracking = |branch: &str| {
        backend::count(
            &url,
            &format!(
                "SELECT COUNT(*) FROM config_environment \
                 WHERE name = 'pr-5' AND working LIKE '%\"{branch}\"%'"
            ),
        )
    };
    assert_eq!((tracking("search-5"), tracking("renamed")), (0, 1));
}

/// News of a merge and a sweep settling the removal of the merged pull request's closed
/// preview both finish: each locks the plan before the preview or its Parent.
#[test]
fn a_merge_and_the_sweep_settling_its_closed_preview_both_finish() {
    let Some((store, who, url)) = shop_on_postgres() else {
        return;
    };
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let opened = opened(&store, &who, (5, "feature"), &trusted);
    run(&store, &opened.admitted.first().unwrap().deployment.id);
    pull(
        &store,
        &who,
        facts(5, false, "feature", "2026-01-02T00:00:00Z"),
        &trusted,
    );
    let mut merged = facts(5, false, "feature", "2026-01-03T00:00:00Z");
    merged.merge_commit = Some(backend::sha(HEAD));
    let mut gate = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let held = hold_parent(&mut gate);
    let sweep = SystemEvent::Sweep(Sweep { now: 0 });
    let news = SystemEvent::PullRequest(merged);

    let (swept, news) = std::thread::scope(|threads| {
        let swept = threads.spawn(|| store.system(&who.organization, &sweep, &trusted));
        queued(&url, 1);
        let news = threads.spawn(|| store.system(&who.organization, &news, &trusted));
        queued(&url, 2);
        held.rollback().unwrap();
        (swept.join().unwrap(), news.join().unwrap())
    });

    assert!(swept.is_ok() && news.is_ok(), "{swept:?} {news:?}");
}

#[test]
fn a_live_preview_never_closes_for_sitting_idle_and_redeploys_on_each_push() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let first = opened(&store, &who, (5, "main"), &trusted);
    run(&store, &first.admitted.first().unwrap().deployment.id);

    let idle = observe(
        &store,
        &who,
        SystemEvent::Sweep(Sweep { now: i64::MAX }),
        &trusted,
    );

    assert!(idle.closing.is_empty(), "{idle:?}");
    assert!(previewing(&store, &who, 5));
    let next = "2222222222222222222222222222222222222222";
    let pushed = observe(
        &store,
        &who,
        SystemEvent::BranchHead(BranchHead {
            repository_id: backend::repo_id(11),
            branch: backend::git_branch("search-5"),
            base: None,
            head: Some(backend::sha(next)),
            changed: None,
        }),
        &trusted,
    );
    let redeployed = pushed.admitted.first().map(|admitted| &admitted.deployment);
    assert_eq!(
        redeployed
            .and_then(|deployment| deployment.push.as_ref())
            .map(|push| push.head.as_str()),
        Some(next),
        "{pushed:?}"
    );
}

/// Removing a whole Project deletes its row, which deletes its PR plans, after it locked
/// every member. News of a pull request locks the plan before `staging`, so the removal
/// must hold the plan before any member.
#[test]
fn removing_a_project_and_news_of_its_pull_request_both_finish() {
    let Some((store, who, url)) = shop_on_postgres() else {
        return;
    };
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let remove = ployz_store::Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: None,
        version: None,
        accept_volume_loss: Default::default(),
        every_copy: Default::default(),
    };
    let event = SystemEvent::PullRequest(facts(5, true, "feature", "2026-01-01T00:00:00Z"));
    // Hold the Project's delete until news of the pull request queues behind the removal.
    let mut gate = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    gate.batch_execute(
        "CREATE FUNCTION pause_project_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         PERFORM pg_advisory_xact_lock(54322); RETURN OLD; END $$; \
         CREATE TRIGGER pause_project_delete BEFORE DELETE ON config_project \
         FOR EACH ROW EXECUTE FUNCTION pause_project_delete(); \
         SELECT pg_advisory_lock(54322)",
    )
    .unwrap();

    let (removed, news) = std::thread::scope(|threads| {
        let removed = threads.spawn(|| store.write_trusted(&who, &remove, &trusted).map(drop));
        queued(&url, 1);
        let news = threads.spawn(|| store.system(&who.organization, &event, &trusted).map(drop));
        queued(&url, 2);
        gate.batch_execute("SELECT pg_advisory_unlock(54322)")
            .unwrap();
        (removed.join().unwrap(), news.join().unwrap())
    });

    assert!(removed.is_ok() && news.is_ok(), "{removed:?} {news:?}");
}

/// A sweep that settles a Project's removal deletes the Project, and so its plans, which
/// it locked before any Environment. A change to a plan locks the Project before the
/// plan, so the sweep must hold the Project before the plans.
#[test]
fn a_sweep_deleting_a_project_and_a_change_to_its_plan_both_finish() {
    let Some((store, who, url)) = shop_on_postgres() else {
        return;
    };
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let opened = opened(&store, &who, (5, "feature"), &trusted);
    run(&store, &opened.admitted.first().unwrap().deployment.id);
    let remove = |version: Option<String>| ployz_store::Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: None,
        version,
        accept_volume_loss: Default::default(),
        every_copy: Default::default(),
    };
    let asked = store
        .write_trusted(&who, &remove(None), &trusted)
        .unwrap_err();
    let version = asked
        .details
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap()
        .to_owned();
    let removing = store
        .write_trusted(&who, &remove(Some(version)), &trusted)
        .unwrap();
    let ployz_store::Removal::Removing { left, .. } = removing else {
        panic!("removed at once");
    };
    let taking_off = left
        .iter()
        .find_map(|left| left.deployment.as_ref())
        .unwrap();
    run(&store, &taking_off.id);
    let sweep = SystemEvent::Sweep(Sweep { now: 0 });
    let change = SetPrPlan {
        include_bots: Some(true),
        ..plan("staging")
    };
    let mut gate = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let held = hold_parent(&mut gate);

    let (swept, changed) = std::thread::scope(|threads| {
        // The sweep holds every plan and waits on `staging`, before it deletes the Project.
        let swept = threads.spawn(|| store.system(&who.organization, &sweep, &trusted).map(drop));
        queued(&url, 1);
        let changed = threads.spawn(|| save(&store, &who, &change));
        queued(&url, 2);
        held.rollback().unwrap();
        (swept.join().unwrap(), changed.join().unwrap())
    });

    assert!(swept.is_ok(), "{swept:?}");
    // The Project is gone, so the change finds nothing to change: refused, not deadlocked.
    let refused = changed.unwrap_err();
    assert_ne!(
        refused.code,
        ployz_core::RpcErrorCode::Unavailable,
        "{refused:?}"
    );
}

/// Making `staging` the default Environment writes its Project. News of a pull request
/// holds the plan and waits on `staging`, the plan's start-from, and a change to the
/// plan holds the Project and waits on the plan: the new default must lock the Project
/// before `staging`.
#[test]
fn a_new_default_environment_news_of_a_pull_request_and_a_plan_change_all_finish() {
    let Some((store, who, url)) = shop_on_postgres() else {
        return;
    };
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let default = ployz_store::SetDefaultEnvironment {
        environment: at("staging"),
    };
    let news = SystemEvent::PullRequest(facts(5, true, "feature", "2026-01-01T00:00:00Z"));
    let change = SetPrPlan {
        include_bots: Some(true),
        ..plan("staging")
    };
    // Stop the new default at the removal marks it reads, after it locked what it locks first.
    let mut gate = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut held = gate.transaction().unwrap();
    held.batch_execute("LOCK TABLE config_removal_mark IN ACCESS EXCLUSIVE MODE")
        .unwrap();

    let (defaulted, news, changed) = std::thread::scope(|threads| {
        let defaulted = threads.spawn(|| store.write(&who, &default).map(drop));
        queued(&url, 1);
        let news = threads.spawn(|| store.system(&who.organization, &news, &trusted).map(drop));
        queued(&url, 2);
        let changed = threads.spawn(|| save(&store, &who, &change));
        queued(&url, 3);
        held.rollback().unwrap();
        (
            defaulted.join().unwrap(),
            news.join().unwrap(),
            changed.join().unwrap(),
        )
    });

    assert!(
        defaulted.is_ok() && news.is_ok() && changed.is_ok(),
        "{defaulted:?} {news:?} {changed:?}"
    );
}

/// Cancelling the Deployment that takes a preview off its Server stops its Project's
/// whole removal, so it locks every member; the sweep settling that removal locks them
/// too. Both must take them in ID order, after the Project.
#[test]
fn cancelling_a_project_removal_s_deployment_and_the_sweep_both_finish() {
    let Some((store, who, url)) = shop_on_postgres() else {
        return;
    };
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let opened = opened(&store, &who, (5, "feature"), &trusted);
    run(&store, &opened.admitted.first().unwrap().deployment.id);
    let remove = |version: Option<String>| ployz_store::Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: None,
        version,
        accept_volume_loss: Default::default(),
        every_copy: Default::default(),
    };
    let asked = store
        .write_trusted(&who, &remove(None), &trusted)
        .unwrap_err();
    let version = asked
        .details
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap()
        .to_owned();
    let removing = store
        .write_trusted(&who, &remove(Some(version)), &trusted)
        .unwrap();
    let ployz_store::Removal::Removing { left, .. } = removing else {
        panic!("removed at once");
    };
    let taking_off = left
        .iter()
        .find_map(|left| left.deployment.as_ref())
        .unwrap()
        .id
        .clone();
    let cancel = ployz_store::Cancel {
        deployment: taking_off.clone(),
    };
    let sweep = SystemEvent::Sweep(Sweep { now: 0 });
    // Stop the cancel at the Deployment's row, after it locked what it locks first.
    let mut gate = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut held = gate.transaction().unwrap();
    held.execute(
        "SELECT 1 FROM config_deployment WHERE id = $1 FOR UPDATE",
        &[&taking_off.as_str()],
    )
    .unwrap();

    let (cancelled, swept) = std::thread::scope(|threads| {
        let cancelled = threads.spawn(|| store.write(&who, &cancel).map(drop));
        queued(&url, 1);
        let swept = threads.spawn(|| store.system(&who.organization, &sweep, &trusted).map(drop));
        queued(&url, 2);
        held.rollback().unwrap();
        (cancelled.join().unwrap(), swept.join().unwrap())
    });

    assert!(
        cancelled.is_ok() && swept.is_ok(),
        "{cancelled:?} {swept:?}"
    );
}

#[test]
fn a_deploy_of_a_head_the_pull_request_moved_past_is_refused() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let mut pushed = facts(5, true, "main", "2026-01-01T00:00:00Z");
    pushed.head = backend::sha("2222222222222222222222222222222222222222");
    pull(&store, &who, pushed, &trusted);

    let refused = deploy(&store, &who, 5, &trusted).unwrap_err();

    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        refused.message,
        "This pull request has a newer commit. Reload to deploy it."
    );
    assert_eq!(preview_of(&store, &who, 5), None);
}

#[test]
fn closing_a_preview_frees_its_place_and_its_pull_request_can_deploy_again() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let first = opened(&store, &who, (5, "main"), &trusted);
    run(&store, &first.admitted.first().unwrap().deployment.id);
    let was = preview_of(&store, &who, 5).unwrap();
    pull(
        &store,
        &who,
        facts(6, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
    assert!(
        deploy(&store, &who, 6, &trusted).is_err(),
        "the cap is full"
    );

    let close = ClosePreview {
        project: None,
        environment: EnvironmentName::parse("pr-5").unwrap(),
    };
    store.write(&who, &close).unwrap();

    let closed = OpenPreviewsQuery {
        namespaces: vec![was.namespace().clone()],
    };
    assert!(store.read(&who, &closed).unwrap().open.is_empty());
    let swept = observe(&store, &who, SystemEvent::Sweep(Sweep { now: 0 }), &trusted);
    assert!(
        swept
            .closing
            .iter()
            .any(|closing| closing.name.as_str() == "pr-5"),
        "Cloud takes it off its Server: {swept:?}"
    );
    assert_eq!(deploy(&store, &who, 6, &trusted).unwrap().admitted.len(), 1);
    let close_six = ClosePreview {
        environment: EnvironmentName::parse("pr-6").unwrap(),
        ..close
    };
    store.write(&who, &close_six).unwrap();
    let again = deploy(&store, &who, 5, &trusted).unwrap();
    assert_eq!(again.admitted.len(), 1, "{again:?}");
    assert!(previewing(&store, &who, 5));
}

/// Pull request `number`'s row in `view`.
fn row(view: &ployz_store::PreviewsView, number: u64) -> &ployz_store::PreviewRow {
    view.pull_requests
        .iter()
        .find(|row| row.number == backend::pr_number(number))
        .unwrap()
}

/// Why pull request `number` of `view` has no preview.
fn why_not(view: &ployz_store::PreviewsView, number: u64) -> Option<NotDeployed> {
    match &row(view, number).preview {
        PreviewOutcome::NotDeployed { why } => why.clone(),
        other @ (PreviewOutcome::Deploying { .. }
        | PreviewOutcome::Live { .. }
        | PreviewOutcome::Closed) => panic!("{other:?}"),
    }
}

#[test]
fn each_pull_request_shows_its_preview_or_why_it_has_none() {
    let (store, who) = shop();
    previews(&store, &who, 2);
    let trusted = homes(&['a']);
    let query = PreviewsQuery {
        project: None,
        repository: backend::repo_name("acme/web"),
        number: None,
    };
    let live = opened(&store, &who, (5, "main"), &trusted);
    run(&store, &live.admitted.first().unwrap().deployment.id);
    let mut stranger = facts(6, true, "main", "2026-01-01T00:00:00Z");
    stranger.author = "mallory".into();
    stranger.author_id = Some(ployz_store::GithubUserId::parse(999_999).unwrap());
    pull(&store, &who, stranger, &trusted);
    pull(
        &store,
        &who,
        facts(7, true, "staging", "2026-01-01T00:00:00Z"),
        &trusted,
    );
    let deploying = opened(&store, &who, (8, "main"), &trusted);
    pull(
        &store,
        &who,
        facts(9, true, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );

    let view = store.read_trusted(&who, &query, &trusted).unwrap();

    assert_eq!((view.live, view.limit), (2, 2));
    let settings = view.settings.as_ref().unwrap();
    assert_eq!(settings.start_from.as_str(), "staging");
    let order: Vec<u64> = view
        .pull_requests
        .iter()
        .map(|row| row.number.get())
        .collect();
    assert_eq!(order, [9, 8, 7, 6, 5], "newest change first");
    let PreviewOutcome::Live {
        environment,
        head,
        server,
        newer,
        ..
    } = &row(&view, 5).preview
    else {
        panic!("{:?}", row(&view, 5));
    };
    assert_eq!(
        (environment.as_str(), head.as_str(), server, newer),
        ("pr-5", HEAD, &Some(machine('a')), &None)
    );
    assert!(matches!(
        &row(&view, 8).preview,
        PreviewOutcome::Deploying { deployment, .. }
            if *deployment == deploying.admitted.first().unwrap().deployment.id
    ));
    let why = |number| why_not(&view, number);
    assert_eq!(why(7), Some(NotDeployed::MergesIntoStartFrom));
    assert_eq!(
        why(6),
        Some(NotDeployed::NotMember {
            author: "mallory".into()
        })
    );
    assert!(!row(&view, 6).member && row(&view, 5).member);
    assert_eq!(why(9), Some(NotDeployed::Full { live: 2, limit: 2 }));

    let pr = PullRequestQuery {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
    };
    let links = store.read(&who, &pr).unwrap().previews;
    assert_eq!(links.len(), 1);
    assert!(matches!(
        links.first().map(|link| &link.outcome),
        Some(PreviewOutcome::Live { .. })
    ));

    // A deploy that failed with nothing live holds no place, and says why.
    let id = &deploying.admitted.first().unwrap().deployment.id;
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(id, &runner).unwrap();
    store
        .record(id, &runner, RunEvidence::NotExecuted("no Servers".into()))
        .unwrap();
    let view = store.read_trusted(&who, &query, &trusted).unwrap();
    let why = |number| why_not(&view, number);
    assert_eq!(view.live, 1);
    assert!(
        matches!(why(8), Some(NotDeployed::Failed { .. })),
        "{:?}",
        row(&view, 8)
    );
    assert_eq!(why(9), None);
}

#[test]
fn a_preview_whose_deploy_failed_deploys_again_as_a_new_one() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let failed = opened(&store, &who, (5, "main"), &trusted);
    let id = &failed.admitted.first().unwrap().deployment.id;
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(id, &runner).unwrap();
    store
        .record(id, &runner, RunEvidence::NotExecuted("no Servers".into()))
        .unwrap();
    let was = preview_of(&store, &who, 5).unwrap();

    let again = deploy(&store, &who, 5, &trusted).unwrap();

    assert_eq!(again.admitted.len(), 1, "{again:?}");
    let closed = OpenPreviewsQuery {
        namespaces: vec![was.namespace().clone()],
    };
    assert!(store.read(&who, &closed).unwrap().open.is_empty());
    assert!(previewing(&store, &who, 5));
}
