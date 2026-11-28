//! The Store calls that dial the Organization's Servers keep only an entry of the
//! Cluster they expect, as a Deploy's does, name the Machines they left, and dial any
//! entry without one.

use super::support::*;
use ployz::connect::ClusterExpectation;
use ployz::context::Connection;
use ployz::sdk::{GithubFinish, GithubKey, GithubLook, GithubStart};
use ployz_core::config::ServiceGitAccess;
use ployz_core::{MachineId, MachineObservation, RpcError, SealingPublicKey, ServiceName};
use ployz_store::testing::backend::PinAs;
use ployz_store::{
    Actor, Admit, AuthorizedRepository, BranchName, BuildLogQuery, BuildStatus, CommitSha,
    ConfigStore, CreateGitService, CreateProject, Deploy, DeploymentId, EnvironmentId,
    EnvironmentRef, GithubBuildId, GithubClaims, GithubGrant, GithubHandOff, GithubReport,
    GithubRun, OrganizationId, ProjectId, ProjectName, RepositoryId, RepositoryName, RunEnd,
    SealingKey, ServiceLineageId, Trusted,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn key(byte: u8) -> SealingPublicKey {
    SealingPublicKey::from_bytes([byte; 32])
}

fn expecting(byte: u8) -> Option<ClusterExpectation> {
    Some(ClusterExpectation {
        sealing_public_key: key(byte),
        allow_unverified: false,
    })
}

#[track_caller]
fn refused_for_another_cluster<T: std::fmt::Debug>(result: Result<T, RpcError>) {
    let error = result.expect_err("only another Cluster's entry answered");
    assert!(
        error.message.contains("belongs to another Cluster"),
        "{error:?}"
    );
}

#[tokio::test]
async fn observing_copies_keeps_only_an_entry_of_the_expected_cluster() {
    let other = DeployService::new(machine('a', "other")).sealed_with(key(9));
    let own = DeployService::new(machine('b', "own")).sealed_with(key(7));
    let (other_address, other_server) = listening(other).await;
    let (own_address, own_server) = listening(own).await;
    let (other, own) = (Connection::tcp(other_address), Connection::tcp(own_address));

    refused_for_another_cluster(
        ployz::sdk::observe_copies(vec![other.clone()], expecting(7)).await,
    );
    let raced = ployz::sdk::observe_copies(vec![other.clone(), own], expecting(7))
        .await
        .expect("the race leaves the other Cluster's entry for its own");
    assert_eq!(raced.foreign, [machine('a', "other").machine.id]);
    let unexpected = ployz::sdk::observe_copies(vec![other], None)
        .await
        .expect("without an expectation any entry answers, as before");
    assert!(unexpected.foreign.is_empty());

    other_server.abort();
    own_server.abort();
}

const RUN: u64 = 555;
const WORKFLOW: &str = "acme/web/.github/workflows/ployz-build.yml@refs/heads/main";

/// A Store Deployment's build of Git Service `web`, its commit pinned.
fn pinned() -> (Arc<ConfigStore>, Actor, GithubBuildId) {
    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
                region: None,
            },
        )
        .unwrap();
    let repository = || RepositoryName::parse("acme/web").unwrap();
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: repository(),
            repository_id: RepositoryId::parse(11).unwrap(),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: BranchName::parse("main").unwrap(),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    let web = ServiceName::parse("web").unwrap();
    let service = CreateGitService {
        id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
        environment: EnvironmentRef::default(),
        name: web.clone(),
        repository: repository(),
        branch: None,
    };
    store.write_trusted(&who, &service, &evidence).unwrap();
    let deployment = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    let deploy = Admit::Deploy(Deploy {
        id: deployment.clone(),
        environment: EnvironmentRef::default(),
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
    });
    store
        .write_trusted(&who, &deploy, &Trusted::default())
        .unwrap();
    let commit = CommitSha::parse("a".repeat(40)).unwrap();
    store
        .pin_as(
            &deployment,
            &BTreeMap::from([(web, commit)]),
            &Trusted::default(),
            ATTEMPT,
        )
        .unwrap();
    store.github_begin(&deployment, ATTEMPT).unwrap();
    let id = GithubBuildId::parse(&format!("{deployment}.web")).unwrap();
    (store, who, id)
}

/// The worker attempt that runs the Deployment's GitHub builds.
const ATTEMPT: &str = "cloud-worker";

/// [`pinned`], handed to GitHub run [`RUN`].
fn dispatched() -> (Arc<ConfigStore>, Actor, GithubBuildId) {
    let (store, who, id) = pinned();
    let run = GithubRun {
        run_id: RUN,
        run_url: format!("https://github.com/acme/web/actions/runs/{RUN}"),
        workflow_ref: WORKFLOW.into(),
        repository: RepositoryName::parse("acme/web").unwrap(),
        installation_id: 7,
        push: Some(false),
        attempt: Some(ATTEMPT.into()),
    };
    store.github_dispatched(&id, &run).unwrap();
    (store, who, id)
}

fn ended(status: BuildStatus, owed: bool) -> GithubFinish {
    GithubFinish::Ended(ployz_store::GithubSettled { status, owed })
}

fn claims() -> GithubClaims {
    GithubClaims {
        repository_id: "11".into(),
        job_workflow_ref: WORKFLOW.into(),
        run_id: RUN.to_string(),
        event_name: "workflow_dispatch".into(),
    }
}

/// A Machine a GitHub build can target: one build platform, and Build Grants.
fn builder(hex: char, name: &str) -> MachineObservation {
    let mut builder = machine(hex, name);
    builder.machine.runtime.architecture = "x86_64".into();
    builder
}

/// `builder(hex, name)`'s daemon, in the Cluster of Sealing key `byte`.
async fn serving(
    hex: char,
    name: &str,
    byte: u8,
) -> (
    Connection,
    MachineId,
    Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let observed = builder(hex, name);
    let id = observed.machine.id;
    let service = DeployService::new(observed)
        .sealed_with(key(byte))
        .minting_grants();
    let mutations = service.mutating_rpcs();
    let (address, server) = listening(service).await;
    (
        Connection::tcp(address).with_machine_id(id),
        id,
        mutations,
        server,
    )
}

#[tokio::test]
async fn a_github_build_starts_only_through_an_entry_of_the_expected_cluster() {
    let (other, other_id, _, other_server) = serving('a', "other", 9).await;
    let (own, _, _, own_server) = serving('b', "own", 7).await;
    let start = |connections, expect| {
        let (store, _, id) = pinned();
        ployz::sdk::github_start(store, id, connections, expect, ATTEMPT.into())
    };
    let dispatch = GithubStart::Dispatch {
        runner: "ubuntu-latest".into(),
    };

    let refused = start(vec![other.clone()], expecting(7)).await.unwrap();
    let GithubStart::Skipped { message } = refused.outcome else {
        panic!("only another Cluster's entry answered: {refused:?}");
    };
    assert!(message.contains("belongs to another Cluster"), "{message}");
    let raced = start(vec![other.clone(), own], expecting(7)).await.unwrap();
    assert_eq!(
        raced.outcome, dispatch,
        "the race reads its own Cluster's entry"
    );
    assert_eq!(raced.foreign, [other_id]);
    let unexpected = start(vec![other], None).await.unwrap();
    assert_eq!(
        unexpected.outcome, dispatch,
        "any entry without an expectation"
    );
    assert!(unexpected.foreign.is_empty());

    other_server.abort();
    own_server.abort();
}

#[tokio::test]
async fn a_github_check_in_mints_only_on_an_entry_of_the_expected_cluster() {
    let (other, other_id, other_mints, other_server) = serving('a', "other", 9).await;
    let (own, _, own_mints, own_server) = serving('b', "own", 7).await;
    let check_in = |store: &Arc<ConfigStore>, id: &GithubBuildId, connections, expect| {
        ployz::sdk::github_check_in(
            Arc::clone(store),
            id.clone(),
            claims(),
            None,
            connections,
            expect,
        )
    };
    let (store, _, id) = dispatched();

    refused_for_another_cluster(check_in(&store, &id, vec![other.clone()], expecting(7)).await);
    assert_eq!(other_mints.load(Ordering::SeqCst), 0);
    let checked_in = check_in(&store, &id, vec![other.clone(), own], expecting(7))
        .await
        .expect("a refused check-in leaves the build to check in again");
    assert!(matches!(checked_in.outcome.key, GithubKey::Grant { .. }));
    assert_eq!(checked_in.foreign, [other_id]);
    assert_eq!(own_mints.load(Ordering::SeqCst), 1);
    assert_eq!(other_mints.load(Ordering::SeqCst), 0);

    let (store, _, id) = dispatched();
    let unexpected = check_in(&store, &id, vec![other], None)
        .await
        .expect("any entry without an expectation");
    assert!(unexpected.foreign.is_empty());
    assert_eq!(other_mints.load(Ordering::SeqCst), 1);

    other_server.abort();
    own_server.abort();
}

#[tokio::test]
async fn a_github_finish_fails_at_once_on_a_grant_machine_of_another_cluster() {
    let (other, other_id, _, other_server) = serving('a', "other", 9).await;
    let (own, own_id, _, own_server) = serving('b', "own", 7).await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_id = builder('c', "silent").machine.id;
    let silent = Connection::tcp(closed.local_addr().unwrap()).with_machine_id(silent_id);
    drop(closed);
    // A build whose run checked in with a grant on `machine`, and ended without pushing.
    let finish = |machine: MachineId, connections: Vec<Connection>, expect| async move {
        let (store, who, id) = dispatched();
        store.github_authorize(&id, &claims()).unwrap();
        let grant = GithubGrant {
            id: "d".repeat(64),
            machine,
            fingerprint: "f".repeat(64),
        };
        store.github_check_in(&id, RUN, &grant).unwrap();
        let finished = ployz::sdk::github_finish(
            Arc::clone(&store),
            id.clone(),
            GithubLook::Ended,
            connections,
            expect,
            ATTEMPT.into(),
            RUN,
        )
        .await
        .unwrap();
        let query = BuildLogQuery {
            deployment: id.deployment.clone(),
            service: id.service.clone(),
        };
        (finished, store.read(&who, &query).unwrap().build.message)
    };
    let connections = vec![other.clone(), own.clone(), silent.clone()];

    let (finished, message) = finish(other_id, connections.clone(), expecting(7)).await;
    assert_eq!(finished, ended(BuildStatus::Failed, false));
    let message = message.expect("the failed build says why");
    assert!(message.contains("belongs to another Cluster"), "{message}");
    for (machine, expect) in [(own_id, expecting(7)), (other_id, None)] {
        let (finished, _) = finish(machine, connections.clone(), expect).await;
        assert_eq!(
            finished,
            ended(BuildStatus::Pending, false),
            "the grant's Machine read the grant: nothing was pushed, so the next Builder takes it"
        );
    }
    let (finished, _) = finish(silent_id, connections, expecting(7)).await;
    assert_eq!(
        finished,
        GithubFinish::Waiting,
        "a grant Machine that doesn't answer is waited for, as before"
    );

    other_server.abort();
    own_server.abort();
}

/// Cloud's start-limit look read the build with no grant, then the run's check-in
/// committed before `github_finish` re-read it. The run started in time, so it keeps
/// the build whatever its grant's Machine answers. Only a run with no grant yet is
/// passed on.
#[tokio::test]
async fn the_start_limit_keeps_a_run_whose_check_in_committed_before_it_settled() {
    let (own, own_id, _, own_server) = serving('b', "own", 7).await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_id = builder('c', "silent").machine.id;
    let silent = Connection::tcp(closed.local_addr().unwrap()).with_machine_id(silent_id);
    drop(closed);
    let connections = vec![own, silent];
    let grant_on = |machine| GithubGrant {
        id: "d".repeat(64),
        machine,
        fingerprint: "f".repeat(64),
    };
    let built = GithubReport {
        from: 0,
        lines: Vec::new(),
        ended: Some(RunEnd::Built {
            platforms: vec!["linux/amd64".into()],
            image_token: None,
        }),
    };

    let cases = [
        (
            own_id,
            false,
            "its Machine read the grant: nothing pushed yet",
        ),
        (silent_id, false, "its Machine doesn't answer"),
        (
            own_id,
            true,
            "it reported Built, and its Machine read nothing pushed",
        ),
    ];
    let mut found = Vec::new();
    for (machine, reported, case) in cases {
        let (store, _, id) = dispatched();
        store.github_check_in(&id, RUN, &grant_on(machine)).unwrap();
        if reported {
            store.github_report(&id, RUN, &built).unwrap();
        }
        let finished = ployz::sdk::github_finish(
            Arc::clone(&store),
            id.clone(),
            GithubLook::StartLimit,
            connections.clone(),
            expecting(7),
            ATTEMPT.into(),
            RUN,
        )
        .await
        .unwrap();
        let held = match store.github_build(&id).unwrap() {
            GithubHandOff::Held(build) => Some((build.status, build.grant)),
            GithubHandOff::PassedOn { .. } => None,
        };
        found.push((case, finished, held));
    }
    let kept = cases
        .map(|(machine, _, case)| {
            (
                case,
                GithubFinish::Waiting,
                Some((BuildStatus::Building, Some(grant_on(machine)))),
            )
        })
        .to_vec();
    assert_eq!(found, kept, "each run that checked in keeps the build");

    let (store, _, id) = dispatched();
    let finished = ployz::sdk::github_finish(
        Arc::clone(&store),
        id.clone(),
        GithubLook::StartLimit,
        connections,
        expecting(7),
        ATTEMPT.into(),
        RUN,
    )
    .await
    .unwrap();
    // Passed on at the start limit, the run Cloud dispatched is owed its cancel.
    assert_eq!(finished, ended(BuildStatus::Pending, true));
    assert_eq!(
        store.github_build(&id).unwrap(),
        GithubHandOff::PassedOn {
            why: "no runner started the build in time".into()
        },
        "a run that never checked in is passed on"
    );

    own_server.abort();
}

/// Shop's `web` from acme/web under an automatic deploy rule a Device signed for
/// linux/amd64, with Deployments `first` and `then` a person asked for, each pinned to
/// one commit.
fn ruled(first: &DeploymentId, then: &DeploymentId) -> (Arc<ConfigStore>, GithubBuildId) {
    use ployz_core::authority::rule::{
        BuildPolicy, Builder, GithubSource, PolicyCache, RuleEvent, RuleService, RuleSource,
        Runner, template,
    };
    use ployz_core::authority::{AutoDeployRule, RuleId, Signed, Signer, SignerKey, Timestamp};
    let (store, who, _) = pinned();
    store
        .write_trusted(
            &who,
            &ployz_store::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
                rule: None,
            },
            &Trusted::default(),
        )
        .unwrap();
    let view = store
        .read(
            &who,
            &ployz_store::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: false,
            },
        )
        .unwrap();
    let (Some(spec), Some(source)) = (view.target.first(), view.sources.first()) else {
        panic!("web builds from acme/web: {view:?}");
    };
    let signer = Signer::from_seed(&[7; 32]);
    let rule = AutoDeployRule {
        proposal: None,
        preview: None,
        cluster: key(7),
        rule: RuleId::from_bytes([9; 16]),
        generation: 1,
        namespace: view.namespace.clone(),
        machines: Vec::new(),
        source: RuleSource::Github(GithubSource {
            repository_id: 11,
            owner_id: 1,
            repository: "acme/web".into(),
            branch: "main".into(),
            workflow: ".github/workflows/ployz-build.yml".into(),
            events: vec![RuleEvent::Push],
            builder: Builder::pinned(),
            runner: Runner::GithubHosted,
        }),
        services: vec![RuleService {
            name: spec.name.clone(),
            template: template(spec, &view.namespace).unwrap(),
            policy: Some(BuildPolicy::derive(
                &source.root_dir,
                &source.build,
                spec,
                ["linux/amd64".to_owned()].into(),
                PolicyCache::Off,
            )),
            secrets: None,
            seeded: None,
        }],
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(ployz_core::DeviceKey::from_bytes(
            *signer.public().as_bytes(),
        )),
    };
    let staged = store
        .read(
            &who,
            &ployz_store::RuleQuery {
                environment: EnvironmentRef::default(),
                staged: true,
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &ployz_store::Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: staged.version,
                message: None,
                accept_volume_loss: Vec::new(),
                rule: Some(ployz_store::SavedRule {
                    rule: Box::new(Signed::sign(&rule, &signer).unwrap()),
                    births: Default::default(),
                    prior: None,
                }),
            },
            &Trusted::default(),
        )
        .unwrap();
    let person = Actor {
        principal: Some(ployz_store::Principal::parse("ada").unwrap()),
        ..who
    };
    let permitted = Trusted {
        permissions: Some(ployz_store::Permission::ALL.iter().copied().collect()),
        ..Trusted::default()
    };
    let web = ServiceName::parse("web").unwrap();
    let commit = CommitSha::parse("a".repeat(40)).unwrap();
    for id in [first, then] {
        let deploy = Admit::Deploy(Deploy {
            id: id.clone(),
            environment: EnvironmentRef::default(),
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
        });
        store.write_trusted(&person, &deploy, &permitted).unwrap();
        store
            .pin_as(
                id,
                &BTreeMap::from([(web.clone(), commit.clone())]),
                &Trusted::default(),
                ATTEMPT,
            )
            .unwrap();
    }
    let then = GithubBuildId::parse(&format!("{then}.web")).unwrap();
    (store, then)
}

#[tokio::test]
async fn a_rule_build_needs_the_services_platforms_even_when_a_server_holds_an_unproven_image() {
    let first = DeploymentId::parse("00000000-0000-4000-8000-000000000201").unwrap();
    let then = DeploymentId::parse("00000000-0000-4000-8000-000000000202").unwrap();
    let (store, id) = ruled(&first, &then);
    // An arm64 Server holds web's image from before the rule, built for this commit; no
    // Server verified it under the rule.
    let mut observed = machine('c', "arm");
    observed.machine.runtime.architecture = "aarch64".into();
    let holder = observed.machine.id;
    let image = format!("sha256:{}", "d".repeat(64));
    let input = store.github_input(&id).unwrap();
    let fingerprint = ployz::sdk::expected_fingerprints(
        input.input,
        BTreeMap::from([(ServiceName::parse("web").unwrap(), input.commit.to_string())]),
    )
    .unwrap()
    .remove(&ServiceName::parse("web").unwrap())
    .unwrap();
    let receipt = serde_json::json!({
        "fingerprint": fingerprint, "variables": "all", "machine_id": holder,
        "image": { "reference": image, "tags": [], "platforms": ["linux/arm64"], "location": "local" },
    });
    let earlier = GithubBuildId::parse(&format!("{first}.web")).unwrap();
    store.github_begin(&first, ATTEMPT).unwrap();
    store
        .github_pass_on(
            &earlier,
            &ployz_store::GithubEnd::Built { receipt },
            None,
            ATTEMPT,
        )
        .unwrap();
    store.github_begin(&then, ATTEMPT).unwrap();
    let mut service = DeployService::new(observed).sealed_with(key(7));
    let builds = BuildFixture::default();
    builds.stores.lock().unwrap().insert(
        holder,
        ployz_core::MachineImages {
            containerd_store: true,
            images: vec![ployz_core::ImageSummary {
                id: image.clone(),
                repo_tags: Vec::new(),
                created: 0,
                size: 0,
                containers: 0,
                platforms: vec!["linux/arm64".into()],
                last_tagged: None,
            }],
            docker_root: None,
        },
    );
    service.builds = Some(Arc::new(builds));
    let (address, server) = listening(service).await;

    let started = ployz::sdk::github_start(
        Arc::clone(&store),
        id,
        vec![Connection::tcp(address).with_machine_id(holder)],
        None,
        ATTEMPT.into(),
    )
    .await
    .unwrap();
    assert_eq!(
        started.outcome,
        GithubStart::Skipped {
            message: "Its automatic deploy rule builds web for linux/amd64, but its servers run \
                      linux/arm64"
                .into()
        },
        "an amd64 build under the rule couldn't run where web runs"
    );

    server.abort();
}
