#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! A push held by Wait for CI is a Deployment waiting for its commit's CI: numbered,
//! listed, and queued in place once every check suite passes.

use std::collections::BTreeMap;

use ployz_core::ServiceName;
use ployz_store::{
    Actor, Automated, Cancel, CiVerdict, ConfigStore, DeploymentQuery, DeploymentStatus,
    DeploymentSummary, DeploymentsQuery, DiffQuery, EnvironmentName, EnvironmentRef, GitPush,
    Outcome, ProjectName, Remove, RenameService, Sweep, SystemEvent, Trusted,
};
use serde_json::json;

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
fn held_push_is_a_numbered_deployment() {
    let (store, who, hold) = waiting();
    assert_eq!(hold.number, 1);
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
    assert_eq!(admitted.deployment.number, hold.number);
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
    assert_eq!(newer.number, 2);
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
    assert_eq!(manual.number, 2);
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
    assert_eq!(in_flight, [2]);
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
