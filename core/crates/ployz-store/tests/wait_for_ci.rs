#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! A push held by Wait for CI is a Deployment waiting for its commit's CI: listed,
//! unnumbered, and queued in place with the next number once every check suite passes.

use std::collections::BTreeMap;

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::tools::{Ask, Place, Step, StoreAt, ToolResult, Write, resume, start};
use ployz_store::{
    Actor, Automated, Cancel, CiVerdict, ConfigStore, DeploymentQuery, DeploymentStatus,
    DeploymentSummary, DeploymentsQuery, DiffQuery, EnvironmentName, EnvironmentRef, GitPush,
    Outcome, ProjectName, PullRequestQuery, Remove, RenameService, Sweep, SystemEvent, Trusted,
};
use serde_json::{Value, json};

mod backend;
mod shop;
use shop::*;

/// `web` waits for CI, and the first push the Store sees, to `H2`, waits.
fn waiting() -> (ConfigStore, Actor, DeploymentSummary) {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let pushed = system(&store, &push(None, Some(H2), None));
    assert_eq!(pushed, Automated::default());
    let [hold] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
    (store, who, hold)
}

fn find(store: &ConfigStore, who: &Actor, hold: &DeploymentSummary) -> DeploymentSummary {
    store
        .read(
            who,
            &DeploymentQuery {
                id: hold.id.clone(),
            },
        )
        .unwrap()
        .deployment
}

fn passed(store: &ConfigStore, head: &str, minute: u8) -> Automated {
    system(store, &suite(1, head, "completed", Some("success"), minute))
}

fn version(store: &ConfigStore, who: &Actor) -> String {
    store
        .read(
            who,
            &DiffQuery {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap()
        .version
}

/// A push to pull request #5's branch, from `base`.
fn push_search(base: Option<&str>, head: &str) -> SystemEvent {
    SystemEvent::BranchHead(ployz_store::BranchHead {
        repository_id: backend::repo_id(11),
        branch: backend::git_branch("search"),
        base: base.map(backend::sha),
        head: Some(backend::sha(head)),
        changed: None,
    })
}

fn reasons(automated: &Automated) -> Vec<&str> {
    automated
        .skipped
        .iter()
        .map(|skipped| skipped.reason.as_str())
        .collect()
}

#[test]
fn held_push_is_an_unnumbered_deployment() {
    let (store, who, hold) = waiting();
    assert_eq!(hold.number, None);
    assert_eq!(hold.label(), &H2[..7]);
    assert_eq!(hold.status, DeploymentStatus::WaitingForCi);
    assert_eq!(hold.ci, Some(CiVerdict::Pending));
    assert!(!hold.in_flight);
    assert_eq!(
        hold.push,
        Some(GitPush {
            repository_id: backend::repo_id(11),
            branch: backend::git_branch("main"),
            head: backend::sha(H2),
        })
    );
    let services: Vec<_> = hold.services.iter().map(ToString::to_string).collect();
    assert_eq!(services, ["web", "api"]);
    assert_eq!(deployments(&store, &who), [hold]);
}

#[test]
fn replayed_push_writes_nothing() {
    let (store, who, hold) = waiting();
    let replayed = system(&store, &push(None, Some(H2), None));
    assert_eq!(replayed, Automated::default());
    assert_eq!(deployments(&store, &who), [hold]);
}

#[test]
fn passing_suite_promotes_in_place() {
    let (store, who, hold) = waiting();
    set(&store, &who, "web.env.X", json!("1"));
    publish(&store, &who);
    let automated = passed(&store, H2, 10);
    let [admitted] = <[_; 1]>::try_from(automated.admitted.clone()).unwrap();
    assert_eq!(admitted.deployment.id, hold.id);
    assert_eq!(admitted.deployment.number, Some(1));
    let promoted = find(&store, &who, &hold);
    assert_eq!(promoted.status, DeploymentStatus::Queued);
    assert_eq!(promoted.ci, None);
    assert!(!promoted.held);
    // Frozen against the Saved revision when it passed, not when it was pushed.
    assert_eq!(
        deployed(&store, &who, &automated),
        vec![(vec!["web".to_owned(), "api".to_owned()], 2)]
    );
    assert_eq!(pinned(&store, &automated, "web").as_deref(), Some(H2));
    assert_eq!(deployments(&store, &who).len(), 1);
}

#[test]
fn replayed_success_admits_nothing() {
    let (store, who, _) = waiting();
    assert_eq!(passed(&store, H2, 10).admitted.len(), 1);
    assert_eq!(passed(&store, H2, 10), Automated::default());
    assert_eq!(deployments(&store, &who).len(), 1);
}

#[test]
fn failing_suite_reads_ci_failed() {
    let (store, who, hold) = waiting();
    let failed = system(&store, &suite(1, H2, "completed", Some("failure"), 10));
    assert_eq!(failed, Automated::default());
    let listed = find(&store, &who, &hold);
    assert_eq!(listed.ci, Some(CiVerdict::Failed));
    assert_eq!(
        DeploymentSummary {
            ci: Some(CiVerdict::Pending),
            ..listed
        },
        hold
    );
}

#[test]
fn passing_rerun_after_failure_deploys() {
    let (store, who, hold) = waiting();
    system(&store, &suite(1, H2, "completed", Some("failure"), 10));
    let automated = passed(&store, H2, 20);
    assert_eq!(automated.admitted[0].deployment.id, hold.id);
    assert_eq!(find(&store, &who, &hold).status, DeploymentStatus::Queued);
}

#[test]
fn newer_push_supersedes_hold() {
    let (store, who, hold) = waiting();
    system(&store, &push(Some(H2), Some(H3), None));
    assert_eq!(
        find(&store, &who, &hold).status,
        DeploymentStatus::Superseded
    );
    let [newer] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
    assert_eq!(newer.number, None);
    assert_eq!(newer.push.unwrap().head, backend::sha(H3));
    assert_eq!(passed(&store, H2, 10), Automated::default());
}

#[test]
fn manual_deploy_supersedes_hold() {
    let (store, who, hold) = waiting();
    let id = backend::admit(&store, &who, "production", 1);
    assert_eq!(
        find(&store, &who, &hold).status,
        DeploymentStatus::Superseded
    );
    assert_eq!(passed(&store, H2, 10), Automated::default());
    let manual = store
        .read(&who, &DeploymentQuery { id })
        .unwrap()
        .deployment;
    assert_eq!(manual.number, Some(1));
}

#[test]
fn hold_is_not_in_flight_for_changes() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let before = store
        .read(
            &who,
            &DiffQuery {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    system(&store, &push(None, Some(H2), None));
    assert_eq!(held(&store, &who).len(), 1);
    let after = store
        .read(
            &who,
            &DiffQuery {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    assert_eq!(after.version, before.version);
    assert_eq!(after.changes, before.changes);
}

#[test]
fn hold_is_not_the_pending_attempt() {
    let (store, who, hold) = waiting();
    let view = store
        .read(
            &who,
            &DeploymentQuery {
                id: hold.id.clone(),
            },
        )
        .unwrap();
    assert!(!view.deployment.in_flight);
    assert!(view.nodes.is_empty());
    // A manual Deploy is the pending attempt, and the only Deployment in flight.
    backend::admit(&store, &who, "production", 1);
    let in_flight: Vec<_> = deployments(&store, &who)
        .into_iter()
        .filter(|deployment| deployment.in_flight)
        .map(|deployment| deployment.number)
        .collect();
    assert_eq!(in_flight, [Some(1)]);
}

#[test]
fn hold_holds_no_slot() {
    let (store, who, _) = waiting();
    let id = backend::admit(&store, &who, "production", 1);
    let manual = store
        .read(&who, &DeploymentQuery { id })
        .unwrap()
        .deployment;
    assert_eq!(manual.status, DeploymentStatus::Queued);
    backend::run(&store, &manual.id);
}

#[test]
fn hold_is_never_unclaimed() {
    let (store, _, _) = waiting();
    assert!(store.unclaimed(i64::MAX).unwrap().is_empty());
}

/// Holds on two branches pass CI in the other order from how they were pushed. Each
/// is numbered as it queues, so numbers follow what ran, and the Environment's live
/// revision is the later-applied one's.
/// A shop with web on main and api on dev, both waiting for CI: web's hold on `H2`
/// pushed first, then api's on `H3`. Returns the two holds, web's first.
fn two_branches_held() -> (ConfigStore, Actor, DeploymentSummary, DeploymentSummary) {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    set(&store, &who, "api.waitForCi", json!(true));
    store
        .write_trusted(
            &who,
            &ployz_store::Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![ployz_store::Change::Set {
                    path: ployz_store::SettingPath::parse("api.branch").unwrap(),
                    value: json!("dev"),
                }],
            },
            &Trusted {
                repositories: vec![ployz_store::AuthorizedRepository {
                    repository: backend::repo_name("acme/web"),
                    repository_id: backend::repo_id(11),
                    access: ployz_core::config::ServiceGitAccess::GithubInstallation {
                        installation_id: 7,
                    },
                    default_branch: backend::git_branch("main"),
                    branches: vec![backend::git_branch("dev")],
                }],
                ..Trusted::default()
            },
        )
        .unwrap();
    publish(&store, &who);
    system(&store, &push(None, Some(H2), None));
    system(
        &store,
        &SystemEvent::BranchHead(ployz_store::BranchHead {
            repository_id: backend::repo_id(11),
            branch: backend::git_branch("dev"),
            base: None,
            head: Some(backend::sha(H3)),
            changed: None,
        }),
    );
    // The two list newest hold first; pick each by its commit all the same.
    let holds = held(&store, &who);
    assert_eq!(holds.len(), 2);
    let by_head = |head: &str| {
        holds
            .iter()
            .find(|hold| hold.push.as_ref().map(|push| &push.head) == Some(&backend::sha(head)))
            .cloned()
            .unwrap()
    };
    let (web_hold, api_hold) = (by_head(H2), by_head(H3));
    assert_eq!((web_hold.number, api_hold.number), (None, None));
    (store, who, web_hold, api_hold)
}

#[test]
fn holds_number_in_the_order_they_pass_ci() {
    let (store, who, web_hold, api_hold) = two_branches_held();
    assert_eq!(
        (web_hold.label(), api_hold.label()),
        (H2[..7].to_owned(), H3[..7].to_owned())
    );

    // api's CI passes first: it queues as #1, at the current Saved revision.
    let [api] = <[_; 1]>::try_from(passed(&store, H3, 10).admitted).unwrap();
    assert_eq!(api.deployment.id, api_hold.id);
    assert_eq!(api.deployment.number, Some(1));
    backend::run(&store, &api.deployment.id);
    let api_saved = find(&store, &who, &api_hold).saved;

    set(&store, &who, "web.env.X", json!("1"));
    publish(&store, &who);
    let [web] = <[_; 1]>::try_from(
        system(&store, &suite(2, H2, "completed", Some("success"), 11)).admitted,
    )
    .unwrap();
    assert_eq!(web.deployment.id, web_hold.id);
    assert_eq!(web.deployment.number, Some(2));
    backend::run(&store, &web.deployment.id);
    let web_saved = find(&store, &who, &web_hold).saved;
    assert!(web_saved > api_saved);

    // Each moved to the top of the list as it queued, so the list runs in number
    // order, web's #2 first, however the holds were pushed.
    let numbers: Vec<_> = deployments(&store, &who)
        .iter()
        .map(|deployment| (deployment.id.clone(), deployment.number))
        .collect();
    assert_eq!(
        numbers,
        [
            (web_hold.id.clone(), Some(2)),
            (api_hold.id.clone(), Some(1))
        ]
    );
    let history = store
        .read(
            &who,
            &ployz_store::HistoryQuery {
                environment: EnvironmentRef::default(),
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(history.live, Some(web_saved));
}

#[test]
fn hold_has_no_builds() {
    let (store, who, hold) = waiting();
    let view = store.read(&who, &DeploymentQuery { id: hold.id }).unwrap();
    assert!(view.builds.is_empty());
    assert!(view.preview.is_none());
}

#[test]
fn cancel_hold() {
    let (store, who, hold) = waiting();
    store
        .write(
            &who,
            &Cancel {
                deployment: hold.id.clone(),
            },
        )
        .unwrap();
    let cancelled = find(&store, &who, &hold);
    assert_eq!(cancelled.status, DeploymentStatus::Cancelled);
    assert!(cancelled.ended_at.is_some());
    assert!(held(&store, &who).is_empty());
}

#[test]
fn a_member_cancelling_a_hold_in_protected_production_asks_an_admin() {
    let (store, system, hold) = waiting();
    let linus = Actor {
        principal: Some(ployz_store::Principal::parse("linus").unwrap()),
        ..system.clone()
    };
    let member = Trusted {
        approval: ployz_store::Approval::Required,
        permissions: Some(
            ployz_store::Permission::ALL
                .iter()
                .copied()
                .filter(|held| *held != ployz_store::Permission::EnvironmentsProtect)
                .collect(),
        ),
        ..Trusted::default()
    };
    let refused = store
        .write_trusted(
            &linus,
            &Cancel {
                deployment: hold.id.clone(),
            },
            &member,
        )
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(refused.details["protected"], json!(true));
    assert_eq!(
        find(&store, &system, &hold).status,
        DeploymentStatus::WaitingForCi
    );
}

#[test]
fn cancel_then_late_success_mints_nothing() {
    let (store, who, hold) = waiting();
    store
        .write(
            &who,
            &Cancel {
                deployment: hold.id.clone(),
            },
        )
        .unwrap();
    assert_eq!(passed(&store, H2, 10), Automated::default());
    assert_eq!(deployments(&store, &who).len(), 1);
}

#[test]
fn no_servers_at_pass_cancels_with_reason() {
    let (store, who, hold) = waiting();
    let serverless = Trusted {
        servers: Some(0),
        ..backend::members()
    };
    let Ok(ployz_store::Written::Automated(automated)) = store.system(
        &org(),
        &suite(1, H2, "completed", Some("success"), 10),
        &serverless,
    ) else {
        panic!("a system event writes Automated")
    };
    assert!(automated.admitted.is_empty());
    let [reason] = <[_; 1]>::try_from(reasons(&automated)).unwrap();
    let cancelled = find(&store, &who, &hold);
    assert_eq!(cancelled.status, DeploymentStatus::Cancelled);
    assert_eq!(
        cancelled.outcome,
        Some(Outcome::NotExecuted {
            reason: reason.to_owned(),
            cause: Vec::new(),
            needs_upload: Vec::new(),
            code: None,
        })
    );
}

/// Pull request environments from `production`, and pull request #5 from `search`
/// at `H2`, opened by a member or not; what opening it admitted.
fn pull_request_by(store: &ConfigStore, who: &Actor, member: bool) -> Automated {
    store
        .write(
            who,
            &ployz_store::SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let opened = system(store, &pull_request(member, true));
    system(store, &push_search(None, H2));
    opened
}

fn pull_request(member: bool, open: bool) -> SystemEvent {
    let (author, author_id) = match member {
        true => ("ada", backend::ada()),
        false => ("eve", ployz_store::GithubUserId::parse(42).unwrap()),
    };
    SystemEvent::PullRequest(ployz_store::PullRequest {
        repository_id: backend::repo_id(11),
        number: backend::pr_number(5),
        title: "Add search".into(),
        author: author.into(),
        author_id: Some(author_id),
        bot: false,
        head_branch: backend::git_branch("search"),
        head_repository_id: Some(backend::repo_id(11)),
        head: backend::sha(H2),
        target_branch: backend::git_branch("main"),
        commits: 1,
        open,
        merge_commit: None,
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse(match open {
            true => "2026-09-29T10:01:00Z",
            false => "2026-09-29T11:00:00Z",
        })
        .unwrap(),
    })
}

/// The Deployments of the Environment `deployment` deployed into.
fn beside(
    store: &ConfigStore,
    who: &Actor,
    deployment: &DeploymentSummary,
) -> Vec<DeploymentSummary> {
    let environment = store
        .read(
            who,
            &DeploymentQuery {
                id: deployment.id.clone(),
            },
        )
        .unwrap()
        .environment;
    store
        .read(
            who,
            &DeploymentsQuery {
                environment: EnvironmentRef {
                    project: None,
                    environment: Some(environment.name),
                },
                ..DeploymentsQuery::default()
            },
        )
        .unwrap()
        .deployments
}

#[test]
fn needs_approval_at_push_holds_nothing() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    pull_request_by(&store, &who, false);
    let approved = store
        .write(
            &who,
            &ployz_store::ApprovePullRequest {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
                head: backend::sha(H2),
            },
        )
        .unwrap();
    let opened = approved.admitted[0].deployment.clone();
    backend::run(&store, &opened.id);

    let pushed = system(&store, &push_search(Some(H2), H3));
    assert_eq!(reasons(&pushed), ["Needs approval"]);
    let listed = beside(&store, &who, &opened);
    assert!(
        listed
            .iter()
            .all(|deployment| deployment.status != DeploymentStatus::WaitingForCi),
        "{listed:?}"
    );
}

#[test]
fn closing_a_pull_request_cancels_its_hold() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let opened = pull_request_by(&store, &who, true).admitted[0]
        .deployment
        .clone();
    backend::run(&store, &opened.id);
    system(&store, &push_search(Some(H2), H3));
    let hold = beside(&store, &who, &opened)
        .into_iter()
        .find(|deployment| deployment.status == DeploymentStatus::WaitingForCi)
        .unwrap();

    system(&store, &pull_request(true, false));
    assert_eq!(
        find(&store, &who, &hold).status,
        DeploymentStatus::Cancelled
    );
    assert_eq!(passed(&store, H3, 10), Automated::default());
}

#[test]
fn sweep_leaves_a_hold_waiting() {
    let (store, who, hold) = waiting();
    system(&store, &SystemEvent::Sweep(Sweep { now: i64::MAX / 2 }));
    assert_eq!(held(&store, &who), [hold]);
}

#[test]
fn teardown_cancels_hold() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    backend::deploy(&store, &who, "production", 1);
    system(&store, &push(None, Some(H2), None));
    let [hold] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
    let remove = Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: None,
        version: None,
        accept_volume_loss: BTreeMap::new(),
        every_copy: BTreeMap::new(),
    };
    let refused = store
        .write_trusted(&who, &remove, &Trusted::default())
        .unwrap_err();
    let version = refused.details["version"].as_str().unwrap().to_owned();
    store
        .write_trusted(
            &who,
            &Remove {
                version: Some(version),
                ..remove
            },
            &Trusted::default(),
        )
        .unwrap();
    // The Deployment taking it off replaces the push waiting there.
    assert_eq!(
        find(&store, &who, &hold).status,
        DeploymentStatus::Superseded
    );
    assert_eq!(passed(&store, H2, 10), Automated::default());
}

#[test]
fn cluster_forgotten_cancels_hold() {
    let (store, who, hold) = waiting();
    system(&store, &SystemEvent::ClusterForgotten);
    let cancelled = find(&store, &who, &hold);
    assert_eq!(cancelled.status, DeploymentStatus::Cancelled);
    assert_eq!(passed(&store, H2, 10), Automated::default());
}

#[test]
fn hold_excluded_from_history_head() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let before = version(&store, &who);
    system(&store, &push(None, Some(H2), None));
    assert_eq!(held(&store, &who).len(), 1);
    assert_eq!(version(&store, &who), before);
    // Replaced before it ran, it ends nothing the Changes baseline counts either.
    system(&store, &push(Some(H2), Some(H3), None));
    assert_eq!(version(&store, &who), before);
}

/// "CI failed" reads the suites the same way the rule that lets a hold go does.
#[test]
fn verdict_sql_matches_passed_rule() {
    let conclusions = [
        Some("success"),
        Some("neutral"),
        Some("skipped"),
        Some("failure"),
        Some("cancelled"),
        Some("timed_out"),
        Some("action_required"),
        Some("stale"),
        Some("startup_failure"),
        Some("something_github_adds_later"),
        None,
    ];
    for conclusion in conclusions {
        let (store, who, hold) = waiting();
        let automated = system(&store, &suite(1, H2, "completed", conclusion, 10));
        let went = automated.admitted.len() == 1;
        let failed = find(&store, &who, &hold).ci == Some(CiVerdict::Failed);
        assert!(
            went != failed,
            "{conclusion:?}: went {went}, failed {failed}"
        );
    }
}

/// A hold remembers the Services it selected by ID: renaming one while it waits
/// still promotes it.
#[test]
fn renamed_service_still_promotes() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    set(&store, &who, "web.watchPaths", json!(["web/**"]));
    set(&store, &who, "api.watchPaths", json!(["api/**"]));
    publish(&store, &who);
    // api runs what it saved, so the push has no Saved change of it to ship.
    backend::deploy(&store, &who, "production", 1);
    system(&store, &push(None, Some(H1), None));
    system(
        &store,
        &push(Some(H1), Some(H2), Some(&["web/src/main.rs"])),
    );
    let [hold] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
    let services: Vec<_> = hold.services.iter().map(ToString::to_string).collect();
    assert_eq!(services, ["web"]);
    store
        .write(
            &who,
            &RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                name: ServiceName::parse("frontend").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who);
    let automated = passed(&store, H2, 10);
    assert_eq!(automated.admitted[0].deployment.id, hold.id);
    let targets: Vec<_> = deployed(&store, &who, &automated)
        .into_iter()
        .map(|(services, _)| services)
        .collect();
    assert_eq!(targets, [vec!["frontend".to_owned()]]);
    // Sources go by private DNS name, which a rename keeps.
    let pins: Vec<_> = store
        .sources(&hold.id)
        .unwrap()
        .into_iter()
        .filter_map(|source| source.commit.map(String::from))
        .collect();
    assert_eq!(pins, [H2]);
}

/// A hold never froze what it ships, so a cancelled one has nothing to redeploy.
#[test]
fn cancelled_hold_cannot_be_retried() {
    let (store, who, hold) = waiting();
    assert!(hold.held);
    store
        .write(
            &who,
            &Cancel {
                deployment: hold.id.clone(),
            },
        )
        .unwrap();
    let cancelled = find(&store, &who, &hold);
    assert!(cancelled.held);
    let refused = store
        .write_trusted(
            &who,
            &ployz_store::Admit::Retry(ployz_store::Retry {
                id: ployz_store::DeploymentId::parse("00000000-0000-4000-8000-000000000199")
                    .unwrap(),
                deployment: hold.id.clone(),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted::default(),
        )
        .unwrap_err();
    assert!(
        refused.message.contains("cancelled before its CI passed"),
        "{refused:?}"
    );
    assert_eq!(deployments(&store, &who).len(), 1);
}

/// Drive an agent tool against the Store alone to its result.
fn tool(store: &ConfigStore, who: &Actor, name: &str, input: Value) -> ToolResult {
    let place = Place {
        project: Some(ProjectName::parse("shop").unwrap()),
        environment: None,
        store: StoreAt::Cloud {
            organization: org(),
        },
    };
    let (mut step, mut state) = start(name, input, &place, "call-1");
    loop {
        let asks = match step {
            Step::Read { asks } | Step::Wait { then: asks, .. } => asks,
            Step::Write { write } => match *write {
                Write::Store(command) => {
                    let written = store
                        .write_trusted(who, &command, &Trusted::default())
                        .map(|written| serde_json::to_value(written).unwrap());
                    (step, state) = resume(state, vec![written]);
                    continue;
                }
                Write::Act(act) => panic!("no Cloud in this test: {act:?}"),
            },
            Step::Done { result } => break result,
        };
        let answers = asks
            .into_iter()
            .map(|ask| match ask {
                Ask::Store(query) => store
                    .read(who, &query)
                    .map(|view| serde_json::to_value(view).unwrap()),
                Ask::Cloud(query) => panic!("no Cloud in this test: {query:?}"),
            })
            .collect();
        (step, state) = resume(state, answers);
    }
}

fn ok(result: ToolResult) -> Value {
    match result {
        ToolResult::Ok(value) => value,
        ToolResult::Refused(refusal) => panic!("refused: {refusal:?}"),
    }
}

fn refused(result: ToolResult) -> ployz_store::tools::Refusal {
    match result {
        ToolResult::Ok(value) => panic!("not refused: {value}"),
        ToolResult::Refused(refusal) => *refusal,
    }
}

/// The history is the numbered rows: a hold never stands in for the latest Deployment,
/// not even on Postgres, where NULL numbers sort first under `number DESC`.
#[test]
fn a_cancelled_hold_is_not_the_latest_deployment() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let opened = pull_request_by(&store, &who, true).admitted[0]
        .deployment
        .clone();
    backend::run(&store, &opened.id);
    system(&store, &push_search(Some(H2), H3));
    let hold = beside(&store, &who, &opened)
        .into_iter()
        .find(|deployment| deployment.status == DeploymentStatus::WaitingForCi)
        .unwrap();
    store
        .write(
            &who,
            &Cancel {
                deployment: hold.id.clone(),
            },
        )
        .unwrap();
    assert_eq!(find(&store, &who, &hold).number, None);

    // The pull request's latest Deployment is the one that ran, not the cancelled hold.
    let pr = store
        .read(
            &who,
            &PullRequestQuery {
                repository_id: backend::repo_id(11),
                number: backend::pr_number(5),
            },
        )
        .unwrap();
    let latest = pr.environments[0].deployment.as_ref().unwrap();
    assert_eq!((&latest.id, latest.number), (&opened.id, Some(1)));

    // The list shows the cancelled hold and #1; the hold the push superseded is noise.
    let listed: Vec<_> = beside(&store, &who, &opened)
        .iter()
        .map(|deployment| (deployment.number, deployment.status))
        .collect();
    assert_eq!(
        listed,
        [
            (None, DeploymentStatus::Cancelled),
            (Some(1), DeploymentStatus::Applied)
        ]
    );
}

/// An Environment whose only Deployment is a hold has no latest Deployment to show,
/// cancel, or start by default.
#[test]
fn an_environment_with_only_a_hold_has_no_deployment_yet() {
    let (store, who, _) = waiting();
    let shown = refused(tool(&store, &who, "deployment_show", json!({})));
    assert_eq!(shown.code, RpcErrorCode::NotFound);
    assert_eq!(shown.message, "shop/production has no Deployment yet");
    assert_eq!(
        shown.next,
        [ployz_store::tools::Call::new("deploy", json!({}))]
    );
}

/// With Deployment #1 queued and a hold pushed after it, the agent's default
/// Deployment is #1: `deployment_show {}` shows it and `deployment_cancel {}` cancels it,
/// leaving the hold waiting.
#[test]
fn default_deployment_skips_a_newer_hold() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let queued = backend::admit(&store, &who, "production", 1);
    system(&store, &push(None, Some(H2), None));
    let [hold] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
    assert_eq!(deployments(&store, &who)[0].id, hold.id);

    let shown = ok(tool(&store, &who, "deployment_show", json!({})));
    assert_eq!(shown["deployment"], 1);
    assert_eq!(shown["status"], "queued");

    let cancelled = ok(tool(&store, &who, "deployment_cancel", json!({})));
    assert_eq!(cancelled["deployment"], 1);
    assert_eq!(
        store
            .read(&who, &DeploymentQuery { id: queued })
            .unwrap()
            .deployment
            .status,
        DeploymentStatus::Cancelled
    );
    assert_eq!(held(&store, &who), [hold]);
}

/// Pages walk every listed row, holds included: twenty cancelled holds and one
/// waiting fill a page, and the cursor still reaches the twenty-first. The holds
/// each push superseded before it was cancelled are not listed at all.
#[test]
fn pages_reach_holds_past_the_first_page() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    let heads: Vec<String> = (1..=21).map(|n| format!("{n:040x}")).collect();
    let mut base: Option<&str> = None;
    // Each of the first twenty heads lands on a hold a `stale` push supersedes at
    // once; the hold for the head itself is then cancelled. The last head waits.
    let stale: Vec<String> = (1..=20).map(|n| format!("{n:039x}a")).collect();
    for (n, head) in heads.iter().enumerate() {
        if let Some(stale) = stale.get(n) {
            system(&store, &push(base, Some(stale), None));
            base = Some(stale);
        }
        system(&store, &push(base, Some(head), None));
        base = Some(head);
        if n < 20 {
            let [hold] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
            store
                .write(
                    &who,
                    &Cancel {
                        deployment: hold.id,
                    },
                )
                .unwrap();
        }
    }
    let page = |cursor: Option<String>| {
        store
            .read(
                &who,
                &DeploymentsQuery {
                    limit: Some(20),
                    cursor,
                    ..DeploymentsQuery::default()
                },
            )
            .unwrap()
    };
    let first = page(None);
    assert_eq!(first.deployments.len(), 20);
    // No Deployment was ever numbered: a page of holds leads with none.
    assert_eq!(first.latest, None);
    let second = page(first.next_cursor.clone());
    assert_eq!(second.deployments.len(), 1);
    assert_eq!(second.next_cursor, None);
    // Pushed within one second, the rows still list in push order, newest first:
    // the two pages cover all twenty-one once, the waiting hold leading.
    let rows: Vec<_> = first
        .deployments
        .iter()
        .chain(&second.deployments)
        .collect();
    assert_eq!(
        rows.iter().map(|row| row.label()).collect::<Vec<_>>(),
        heads
            .iter()
            .rev()
            .map(|head| &head[..7])
            .collect::<Vec<_>>()
    );
    let seen: std::collections::BTreeSet<_> = rows.iter().map(|row| row.id.clone()).collect();
    assert_eq!(seen.len(), 21);
    assert!(rows.iter().all(|row| row.number.is_none()));
    assert_eq!(
        rows.iter()
            .filter(|row| row.status == DeploymentStatus::Cancelled)
            .count(),
        20
    );
    assert!(
        rows.iter()
            .all(|row| row.status != DeploymentStatus::Superseded)
    );
    let waiting: Vec<_> = rows
        .iter()
        .filter(|row| row.status == DeploymentStatus::WaitingForCi)
        .map(|row| row.label().to_owned())
        .collect();
    assert_eq!(waiting, [&heads[20][..7]]);

    let bad = store.read(
        &who,
        &DeploymentsQuery {
            limit: Some(20),
            cursor: Some("20".into()),
            ..DeploymentsQuery::default()
        },
    );
    assert_eq!(bad.unwrap_err().code, RpcErrorCode::InvalidArgument);
}

/// Deployments #1 to #3, then a hold on `H2`, which leads the list: a Deployment
/// lists where it was created, and the hold came last. A page of one ends on the
/// hold; `between` then changes the Store before the pages after it are read. The
/// hold, and what those pages list in order.
fn paged_past_a_hold(
    between: impl FnOnce(&ConfigStore, &DeploymentSummary),
) -> (DeploymentSummary, Vec<DeploymentSummary>) {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    for n in 1..=3 {
        let id = backend::admit(&store, &who, "production", n);
        backend::run(&store, &id);
    }
    system(&store, &push(None, Some(H2), None));
    let page = |cursor: Option<String>| {
        store
            .read(
                &who,
                &DeploymentsQuery {
                    limit: Some(1),
                    cursor,
                    ..DeploymentsQuery::default()
                },
            )
            .unwrap()
    };
    let first = page(None);
    let [hold] = <[_; 1]>::try_from(first.deployments).unwrap();
    assert_eq!(hold.status, DeploymentStatus::WaitingForCi);
    // The hold leads the page, and #3 is still the latest numbered Deployment.
    assert_eq!(first.latest.map(|latest| latest.number), Some(Some(3)));
    between(&store, &hold);
    let mut rest = Vec::new();
    let mut cursor = first.next_cursor;
    while let Some(word) = cursor {
        let next = page(Some(word));
        rest.extend(next.deployments);
        cursor = next.next_cursor;
    }
    (hold, rest)
}

/// A hold a page ended on, superseded before the next page is read: the pages
/// after it still reach every numbered Deployment once, and never the hold.
#[test]
fn pages_past_a_superseded_hold() {
    let (hold, rest) = paged_past_a_hold(|store, _| {
        system(store, &push(Some(H2), Some(H3), None));
    });
    assert!(rest.iter().all(|row| row.id != hold.id));
    assert!(
        rest.iter()
            .all(|row| row.status != DeploymentStatus::Superseded)
    );
    let numbered: Vec<_> = rest.iter().filter_map(|row| row.number).collect();
    assert_eq!(numbered, [3, 2, 1]);
    // The hold on H3 is newer than the page bound; at most it reads once.
    let ids: std::collections::BTreeSet<_> = rest.iter().map(|row| &row.id).collect();
    assert_eq!(ids.len(), rest.len());
}

/// A hold a page ended on, promoted before the next page is read: the previous page
/// showed it, so the pages after reach the numbered Deployments once and not it.
#[test]
fn pages_past_a_promoted_hold() {
    let (hold, rest) = paged_past_a_hold(|store, _| {
        let promoted = passed(store, H2, 10);
        assert_eq!(promoted.admitted[0].deployment.number, Some(4));
    });
    assert!(rest.iter().all(|row| row.id != hold.id));
    let numbered: Vec<_> = rest.iter().map(|row| row.number).collect();
    assert_eq!(numbered, [Some(3), Some(2), Some(1)]);
}

/// Web's hold `W` is made, then api's `A`; A passes CI first and queues as #1, a
/// numbered row newer than W in the list. W then passes and queues as #2, which
/// moves it to the top: numbered Deployments list in number order. Pages of one
/// walk #2, #1, each once, and name W the latest throughout. (A manual Deploy
/// supersedes every hold, so only a promoted hold can number ahead of one waiting.)
#[test]
fn a_promoted_hold_lists_above_the_deployments_numbered_while_it_waited() {
    let (store, who, web_hold, api_hold) = two_branches_held();
    let page = |cursor: Option<String>| {
        store
            .read(
                &who,
                &DeploymentsQuery {
                    limit: Some(1),
                    cursor,
                    ..DeploymentsQuery::default()
                },
            )
            .unwrap()
    };
    let [api] = <[_; 1]>::try_from(passed(&store, H3, 10).admitted).unwrap();
    assert_eq!(
        (&api.deployment.id, api.deployment.number),
        (&api_hold.id, Some(1))
    );
    backend::run(&store, &api.deployment.id);
    let waiting = page(None);
    assert_eq!(
        waiting
            .deployments
            .iter()
            .map(|row| &row.id)
            .collect::<Vec<_>>(),
        [&api_hold.id]
    );
    let [web] = <[_; 1]>::try_from(passed(&store, H2, 11).admitted).unwrap();
    assert_eq!(
        (&web.deployment.id, web.deployment.number),
        (&web_hold.id, Some(2))
    );
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let next = page(cursor);
        assert_eq!(
            next.latest.as_ref().map(|latest| &latest.id),
            Some(&web_hold.id)
        );
        rows.extend(next.deployments);
        cursor = next.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        rows.iter()
            .map(|row| (&row.id, row.number))
            .collect::<Vec<_>>(),
        [(&web_hold.id, Some(2)), (&api_hold.id, Some(1))]
    );
}

/// A hold on `H2` passes CI and queues as #1; a hold on `H3` is then made and
/// cancelled. A page of one shows only that cancelled hold, #1 sitting behind
/// it, yet the view carries #1 as in flight, and as the latest.
#[test]
fn in_flight_comes_with_the_view_from_past_the_first_page() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    system(&store, &push(None, Some(H2), None));
    let promoted = passed(&store, H2, 10);
    let queued = &promoted.admitted[0].deployment;
    assert_eq!(queued.number, Some(1));
    assert_eq!(queued.status, DeploymentStatus::Queued);
    system(&store, &push(Some(H2), Some(H3), None));
    let [hold] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
    store
        .write(
            &who,
            &Cancel {
                deployment: hold.id.clone(),
            },
        )
        .unwrap();
    let first = store
        .read(
            &who,
            &DeploymentsQuery {
                limit: Some(1),
                cursor: None,
                ..DeploymentsQuery::default()
            },
        )
        .unwrap();
    assert_eq!(
        first
            .deployments
            .iter()
            .map(|row| &row.id)
            .collect::<Vec<_>>(),
        [&hold.id]
    );
    assert!(first.next_cursor.is_some());
    assert_eq!(
        first
            .in_flight
            .iter()
            .map(|row| &row.id)
            .collect::<Vec<_>>(),
        [&queued.id]
    );
    assert_eq!(
        first.latest.as_ref().map(|latest| &latest.id),
        Some(&queued.id)
    );
}

/// A cancelled hold `C` on `H2`, then a hold `H` on `H3` above it, both pushed within
/// a second. A page of one ends on H; H then promotes to #4, which moves it to the
/// top of the list, above the page's cursor, so the pages after reach C, then #3 to
/// #1, and H never again. The list's latest numbered Deployment is H all the same.
#[test]
fn pages_past_a_promoted_hold_above_a_cancelled_one() {
    let (store, who) = shop();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    for n in 1..=3 {
        let id = backend::admit(&store, &who, "production", n);
        backend::run(&store, &id);
    }
    system(&store, &push(None, Some(H2), None));
    let [cancelled] = <[_; 1]>::try_from(held(&store, &who)).unwrap();
    store
        .write(
            &who,
            &Cancel {
                deployment: cancelled.id.clone(),
            },
        )
        .unwrap();
    system(&store, &push(Some(H2), Some(H3), None));
    let page = |cursor: Option<String>| {
        store
            .read(
                &who,
                &DeploymentsQuery {
                    limit: Some(1),
                    cursor,
                    ..DeploymentsQuery::default()
                },
            )
            .unwrap()
    };
    let first = page(None);
    let [hold] = <[_; 1]>::try_from(first.deployments).unwrap();
    assert_eq!(hold.label(), &H3[..7]);
    let promoted = passed(&store, H3, 10);
    assert_eq!(promoted.admitted[0].deployment.number, Some(4));
    let mut rest = Vec::new();
    let mut cursor = first.next_cursor;
    while let Some(word) = cursor {
        let next = page(Some(word));
        assert_eq!(
            next.latest.as_ref().map(|latest| &latest.id),
            Some(&hold.id)
        );
        rest.extend(next.deployments);
        cursor = next.next_cursor;
    }
    assert_eq!(
        rest.iter()
            .map(|row| (row.id == cancelled.id, row.number))
            .collect::<Vec<_>>(),
        [
            (true, None),
            (false, Some(3)),
            (false, Some(2)),
            (false, Some(1))
        ]
    );
    assert!(rest.iter().all(|row| row.id != hold.id));
}
