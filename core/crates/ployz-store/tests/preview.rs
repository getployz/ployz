//! Previews under a PR plan's preview rule, on SQLite and on Postgres (see `backend`): a
//! Parent that can't be production, the cap and its waiting pull requests, the one Server
//! each preview names, and which previews Cloud closes on a Server.

use ployz_core::authority::rule::{Builder, RuleEvent, RuleSource, Runner};
use ployz_core::authority::{AutoDeployRule, PreviewTerms, RuleId, Signed, Signer, SignerKey};
use ployz_core::config::ServiceGitAccess;
use ployz_core::{
    DeployOutcome, DeployPreview, MachineId, Namespace, PreviewNamespace, RpcErrorCode, ServiceName,
};
use ployz_store::testing::backend;
use ployz_store::{
    Actor, Admit, AdmitRemoval, AuthorizedRepository, Automated, ConfigStore, CreateBranch,
    CreateGitService, CreateProject, DeploymentId, EnvironmentId, EnvironmentName, EnvironmentRef,
    OpenPreviewsQuery, OrganizationId, PrPlansQuery, ProjectId, ProjectName, PullRequest,
    RuleQuery, RunEvidence, RunnerId, SealingRecipient, ServiceLineageId, SetEnvironmentProtected,
    SetPrPlan, ShutDown, Signers, Sweep, SystemEvent, Trusted, Written,
};

const HEAD: &str = "1111111111111111111111111111111111111111";

fn uuid(n: u8) -> String {
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

/// Project `shop`: Protected `production` runs `web` from `acme/web`'s `main`, and root
/// `staging` runs `web` and `api` from its `staging` branch, each published.
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
    for environment in ["production", "staging"] {
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
        source: RuleSource {
            repository_id: 11,
            owner_id: 1,
            repository: "acme/web".into(),
            branch: String::new(),
            workflow: ".github/workflows/ployz-build.yml".into(),
            events: vec![RuleEvent::Push],
            builder: Builder::pinned(),
            runner: Runner::GithubHosted,
        },
        services: Vec::new(),
        issued_at: ployz_core::authority::Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes(
            *signer.public().as_bytes(),
        )),
    };
    edit(&mut rule);
    Box::new(Signed::sign(&rule, &signer).unwrap())
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
        rule.source.repository_id = 12;
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
    let into_parent = facts(5, true, "staging", "2026-01-01T00:00:00Z");
    let automated = pull(&store, &who, into_parent, &homes(&['a']));
    assert!(automated.admitted.is_empty());
    assert_eq!(
        skipped(&automated),
        [
            "This pull request merges into the Environment previews start from, so it gets no preview"
        ]
    );
    assert_eq!(preview_of(&store, &who, 5), None);
}

#[test]
fn previews_past_the_cap_wait_and_the_oldest_opens_when_one_closes() {
    let (store, who) = shop();
    previews(&store, &who, 2);
    let trusted = homes(&['a']);
    for number in [5, 6, 7, 8] {
        pull(
            &store,
            &who,
            facts(number, true, "main", "2026-01-01T00:00:00Z"),
            &trusted,
        );
    }
    let open = |numbers: [u64; 4]| numbers.map(|number| preview_of(&store, &who, number).is_some());
    assert_eq!(open([5, 6, 7, 8]), [true, true, false, false]);
    let first = preview_of(&store, &who, 5).unwrap();
    assert!(first.homed_on(&machine('a')));
    assert_eq!(
        first.namespace().as_str().get(..19),
        Some("pv-0909090909090909")
    );

    // A push to a waiting pull request still waits.
    let pushed = pull(
        &store,
        &who,
        facts(8, true, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );
    assert_eq!(skipped(&pushed), ["Waiting for a preview slot"]);

    let closed = pull(
        &store,
        &who,
        facts(5, false, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );
    assert_eq!(
        closed.admitted.len(),
        1,
        "the oldest waiting pull request opens"
    );
    assert_eq!(open([5, 6, 7, 8]), [false, true, true, false]);
}

#[test]
fn a_preview_opens_on_the_emptiest_server_and_again_elsewhere_once_its_server_leaves() {
    let (store, who) = shop();
    previews(&store, &who, 5);
    let both = homes(&['a', 'b']);
    pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &both,
    );
    pull(
        &store,
        &who,
        facts(6, true, "main", "2026-01-01T00:00:00Z"),
        &both,
    );
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
    pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
    pull(
        &store,
        &who,
        facts(6, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
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
        let opened = pull(
            &store,
            &who,
            facts(number, true, "main", "2026-01-01T00:00:00Z"),
            &trusted,
        );
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
    pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &homes(&['a']),
    );
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
fn a_freed_slot_passes_over_a_pull_request_that_gets_no_preview() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    for (number, target) in [(5, "main"), (6, "staging"), (7, "main")] {
        pull(
            &store,
            &who,
            facts(number, true, target, "2026-01-01T00:00:00Z"),
            &trusted,
        );
    }

    let closed = pull(
        &store,
        &who,
        facts(5, false, "main", "2026-01-02T00:00:00Z"),
        &trusted,
    );

    assert_eq!(closed.admitted.len(), 1, "{:?}", skipped(&closed));
    assert!(preview_of(&store, &who, 7).is_some());
}

#[test]
fn a_pull_request_retargeted_into_its_parent_closes_its_preview_and_frees_the_slot() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    for number in [5, 6] {
        pull(
            &store,
            &who,
            facts(number, true, "main", "2026-01-01T00:00:00Z"),
            &trusted,
        );
    }
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
    assert_eq!(retargeted.admitted.len(), 1, "{:?}", skipped(&retargeted));
    assert!(
        preview_of(&store, &who, 6).is_some(),
        "the waiting one opens"
    );
}

#[test]
fn a_preview_a_sweep_may_have_closed_never_opens_again() {
    let (store, who) = shop();
    previews(&store, &who, 5);
    let trusted = homes(&['a']);
    let opened = pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
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
    assert_eq!(reopened.admitted.len(), 1, "{:?}", skipped(&reopened));
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
    pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );

    let both = SystemEvent::PullRequest(facts(5, false, "staging", "2026-01-02T00:00:00Z"));
    store
        .system(&who.organization, &both, &trusted)
        .expect("one observation closes it once");

    assert_eq!(preview_of(&store, &who, 5), None);
}

#[test]
fn a_server_that_returns_a_sweep_later_takes_the_waiting_preview() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &homes(&['a']),
    );
    let sweep = || SystemEvent::Sweep(Sweep { now: 0 });
    let none = observe(&store, &who, sweep(), &homes(&[]));
    assert!(none.admitted.is_empty(), "no Server can take it");

    let back = observe(&store, &who, sweep(), &homes(&['b']));

    assert_eq!(back.admitted.len(), 1, "{:?}", skipped(&back));
}

#[test]
fn a_merge_into_a_branch_nothing_deploys_closes_the_preview_and_frees_its_slot() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    for number in [5, 6] {
        let target = if number == 5 { "feature" } else { "main" };
        pull(
            &store,
            &who,
            facts(number, true, target, "2026-01-01T00:00:00Z"),
            &trusted,
        );
    }
    let asked = OpenPreviewsQuery {
        namespaces: vec![preview_of(&store, &who, 5).unwrap().namespace().clone()],
    };
    let mut merged = facts(5, false, "feature", "2026-01-03T00:00:00Z");
    merged.merge_commit = Some(backend::sha(HEAD));

    let landed = pull(&store, &who, merged, &trusted);

    assert!(store.read(&who, &asked).unwrap().open.is_empty());
    assert_eq!(landed.admitted.len(), 1, "{:?}", skipped(&landed));
    assert!(
        preview_of(&store, &who, 6).is_some(),
        "the waiting one opens"
    );
}

#[test]
fn a_retarget_closes_a_preview_by_the_parent_its_branch_was_made_from() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    pull(
        &store,
        &who,
        facts(5, true, "main", "2026-01-01T00:00:00Z"),
        &trusted,
    );
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
fn approving_a_stranger_s_pull_request_opens_its_preview() {
    let (store, who) = shop();
    previews(&store, &who, 2);
    let trusted = homes(&['a']);
    let mut stranger = facts(5, true, "main", "2026-01-01T00:00:00Z");
    stranger.author_id = Some(ployz_store::GithubUserId::parse(999_999).unwrap());
    pull(&store, &who, stranger, &trusted);
    assert_eq!(preview_of(&store, &who, 5), None, "it waits for a member");

    let approve = ployz_store::ApprovePullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        head: backend::sha(HEAD),
    };
    store.write_trusted(&who, &approve, &trusted).unwrap();

    assert!(preview_of(&store, &who, 5).is_some());
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
    pull(
        &store,
        &who,
        facts(5, true, "feature", "2026-01-01T00:00:00Z"),
        &trusted,
    );
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
fn a_full_cap_costs_the_same_however_many_pull_requests_wait() {
    let (store, who) = shop();
    previews(&store, &who, 1);
    let trusted = homes(&['a']);
    let opened = |number: u64| facts(number, true, "main", "2026-01-01T00:00:00Z");
    // What news of one more pull request costs once `waiting` wait behind the full cap.
    let cost = |first: u64, waiting: u64| {
        for number in first..first + waiting {
            pull(&store, &who, opened(number), &trusted);
        }
        let (asked, statements) =
            backend::statements(|| pull(&store, &who, opened(first + waiting), &trusted));
        assert_eq!(skipped(&asked).len(), 1, "it waits: {asked:?}");
        statements
    };
    pull(&store, &who, opened(1), &trusted);
    assert!(preview_of(&store, &who, 1).is_some());

    let few = cost(2, 2);
    let many = cost(10, 40);

    assert_eq!(few, many, "news of a pull request reads each one waiting");
}

/// Two writes that each close preview `pr-5` as its Server leaves and open the oldest
/// waiting pull request in its place, queued behind a third holding the plan's
/// start-from, both finish and keep the cap:
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
    for number in [5, 6] {
        pull(
            &store,
            &who,
            facts(number, true, "main", "2026-01-01T00:00:00Z"),
            &homes(&['a']),
        );
    }
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
    // The oldest waiting pull request, 5 again, takes the one slot, on b.
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
