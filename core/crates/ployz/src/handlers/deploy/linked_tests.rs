use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::RpcError;
use ployz_store::{
    Actor, Admit, ConfigStore, CreateProject, Deploy, EnvironmentId, EnvironmentRef,
    OrganizationId, ProjectId, ProjectName, SealingKey,
};
use serde_json::{Value, json};

use super::*;
use crate::cloud::account::testing::{fake_cloud, token};
use crate::handlers::store::mint;

const PROPOSAL: &str = "6f1c2b9e-4a7d-4c1e-9a55-0b8f1e2d3c4a";

fn link_answer(cloud: &str) -> StoreCallError {
    let url = format!("{cloud}/cloud/acme/approve/{PROPOSAL}");
    let mut refused = RpcError::new(
        RpcErrorCode::Conflict,
        format!("Waiting for approval at {url}. Nothing deployed yet."),
    );
    refused.details = json!({ "proposal_id": PROPOSAL, "url": url });
    StoreCallError::Refused(refused)
}

/// A Deployment the Store admitted, as Cloud's Store reads it back.
fn admitted_view() -> (DeploymentSummary, Value) {
    let local = ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap();
    let owner = Actor::system(OrganizationId::parse("acme").unwrap());
    local
        .write(
            &owner,
            &CreateProject {
                id: ProjectId::parse(mint()).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(mint()).unwrap(),
                region: None,
            },
        )
        .unwrap();
    let id = DeploymentId::parse(mint()).unwrap();
    local
        .write(
            &owner,
            &Admit::Deploy(Deploy {
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
            }),
        )
        .unwrap();
    let query = DeploymentQuery { id };
    let view = local.read(&owner, &query).unwrap();
    let mut answer = serde_json::to_value(&view).unwrap();
    answer
        .as_object_mut()
        .unwrap()
        .insert("view".into(), json!("deployment"));
    (view.deployment, answer)
}

fn cloud_store<'m>(cloud: &str, matches: &'m clap::ArgMatches) -> Store<'m> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    Store::for_test(Backend::Cloud(runtime, token(cloud)), matches)
}

fn deploy_matches() -> clap::ArgMatches {
    crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"])
        .unwrap()
}

#[test]
fn an_approved_link_hands_back_the_deployment_it_admitted() {
    let (summary, view) = admitted_view();
    let id = summary.id;
    let cloud = fake_cloud(move |route, _| match route {
        route if route == format!("GET /api/cli/proposals/{PROPOSAL}") => (
            200,
            json!({ "proposal": { "status": "approved", "reason": null } }),
        ),
        "POST /api/config/read" => (200, view.clone()),
        other => panic!("unexpected {other}"),
    });
    let matches = deploy_matches();
    let store = cloud_store(&cloud, &matches);
    let deployment = admitted(&store, link_answer(&cloud), &id, Approval::Waits).unwrap();
    assert_eq!(deployment.id, id);
}

#[test]
fn a_denied_link_ends_with_the_denial_and_deploys_nothing() {
    let reads = AtomicUsize::new(0);
    // A second look means the denial read as waiting: it fails at once rather than waiting.
    let cloud = fake_cloud(move |route, _| match route {
        route if route == format!("GET /api/cli/proposals/{PROPOSAL}") => {
            match reads.fetch_add(1, Ordering::SeqCst) {
                0 => (
                    200,
                    json!({ "proposal": { "status": "denied", "reason": "Not during the sale" } }),
                ),
                _ => (400, json!({})),
            }
        }
        other => panic!("a denied proposal read {other}"),
    });
    let matches = deploy_matches();
    let store = cloud_store(&cloud, &matches);
    let id = DeploymentId::parse(mint()).unwrap();
    let Err(StoreCallError::Stopped(failure)) =
        admitted(&store, link_answer(&cloud), &id, Approval::Waits)
    else {
        panic!("a denied proposal admitted a Deployment");
    };
    assert_eq!(
        failure.to_string(),
        "Denied: Not during the sale. Nothing deployed."
    );
}

#[test]
fn an_approval_denied_when_its_deploy_couldn_t_start_ends_with_the_denial() {
    let reads = AtomicUsize::new(0);
    let cloud = fake_cloud(move |route, _| match route {
        route if route == format!("GET /api/cli/proposals/{PROPOSAL}") => {
            match reads.fetch_add(1, Ordering::SeqCst) {
                0 => (
                    200,
                    json!({ "proposal": { "status": "approved", "reason": null } }),
                ),
                1 => (
                    200,
                    json!({ "proposal": { "status": "denied", "reason": "The token that asked for it is gone." } }),
                ),
                _ => (400, json!({})),
            }
        }
        "POST /api/config/read" => (
            404,
            json!({ "error": { "code": "not_found", "message": "No such Deployment.", "details": null } }),
        ),
        other => panic!("unexpected {other}"),
    });
    let matches = deploy_matches();
    let store = cloud_store(&cloud, &matches);
    let id = DeploymentId::parse(mint()).unwrap();
    let Err(StoreCallError::Stopped(failure)) =
        admitted(&store, link_answer(&cloud), &id, Approval::Waits)
    else {
        panic!("a denied proposal admitted a Deployment");
    };
    assert_eq!(
        failure.to_string(),
        "Denied: The token that asked for it is gone. Nothing deployed."
    );
}

#[test]
fn json_detach_or_a_keyed_cli_gets_cloud_s_answer_without_following_it() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let cloud = fake_cloud(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        (500, json!({}))
    });
    let matches = deploy_matches();
    let store = cloud_store(&cloud, &matches);
    let id = DeploymentId::parse(mint()).unwrap();
    for (json, detach, keyed) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let approval = Approval::of(json, detach, keyed);
        let Err(StoreCallError::Stopped(failure)) =
            admitted(&store, link_answer(&cloud), &id, approval)
        else {
            panic!("{approval:?} didn't return Cloud's answer");
        };
        let details = failure.json().get("details").cloned().unwrap_or_default();
        assert_eq!(details.get("proposal_id"), Some(&json!(PROPOSAL)));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

fn unavailable() -> StoreCallError {
    StoreCallError::Refused(RpcError::new(
        RpcErrorCode::Unavailable,
        "Cloud is restarting.",
    ))
}

fn said(answer: Result<DeploymentSummary, StoreCallError>) -> String {
    let Err(StoreCallError::Stopped(failure)) = answer else {
        panic!("the wait admitted a Deployment");
    };
    failure.to_string()
}

#[tokio::test(start_paused = true)]
async fn a_waiting_link_is_followed_through_cloud_s_blips_until_its_deployment_is_admitted() {
    let (deployment, _) = admitted_view();
    let mut looks = Vec::new();
    let mut answers = vec![
        Err(unavailable()),
        Ok(Stand::Waiting),
        Ok(Stand::Approved),
        Err(unavailable()),
        Ok(Stand::Approved),
        Ok(Stand::Admitted(Box::new(deployment.clone()))),
    ]
    .into_iter();
    let mut last = Stand::Waiting;
    let lost = settled(WAIT, &mut last, async |last| {
        looks.push(matches!(last, Stand::Approved));
        answers.next().unwrap()
    })
    .await;
    assert!(lost.is_none());
    assert!(matches!(last, Stand::Admitted(admitted) if admitted.id == deployment.id));
    // Once approved, each look reads the Deployment.
    assert_eq!(looks, [false, false, false, true, true, true]);
}

#[tokio::test(start_paused = true)]
async fn nobody_approving_ends_the_wait_with_nothing_deployed_yet() {
    let started = tokio::time::Instant::now();
    let mut last = Stand::Waiting;
    let lost = settled(WAIT, &mut last, async |_| Ok(Stand::Waiting)).await;
    assert!(lost.is_none());
    assert_eq!(started.elapsed(), WAIT);
    let linked = Linked::of(&link_answer("https://cloud.test")).unwrap();
    assert_eq!(
        said(linked.ended(last, lost)),
        format!(
            "Nothing deployed yet. It waits for approval at https://cloud.test/cloud/acme/approve/{PROPOSAL}."
        )
    );
}

#[tokio::test(start_paused = true)]
async fn a_look_cloud_holds_past_the_deadline_ends_the_wait_on_time() {
    let (deployment, _) = admitted_view();
    let started = tokio::time::Instant::now();
    let mut last = Stand::Waiting;
    let lost = settled(WAIT, &mut last, async |_| {
        tokio::time::sleep(2 * WAIT).await;
        Ok(Stand::Admitted(Box::new(deployment.clone())))
    })
    .await;
    assert!(lost.is_none());
    assert!(matches!(last, Stand::Waiting));
    assert_eq!(started.elapsed(), WAIT);
}

#[tokio::test(start_paused = true)]
async fn losing_cloud_after_the_approval_says_it_was_approved_with_the_link() {
    let mut approved = false;
    let mut last = Stand::Waiting;
    let lost = settled(WAIT, &mut last, async |_| {
        if std::mem::replace(&mut approved, true) {
            Err(unavailable())
        } else {
            Ok(Stand::Approved)
        }
    })
    .await;
    assert!(lost.is_some());
    let linked = Linked::of(&link_answer("https://cloud.test")).unwrap();
    let Err(StoreCallError::Stopped(failure)) = linked.ended(last, lost) else {
        panic!("the wait admitted a Deployment");
    };
    assert_eq!(
        failure.to_string(),
        format!(
            "Approved at https://cloud.test/cloud/acme/approve/{PROPOSAL}, but not started yet."
        )
    );
    let answer = failure.json();
    assert_eq!(answer.get("code"), Some(&json!("unavailable")));
    assert_eq!(
        answer.get("details").and_then(|details| details.get("url")),
        Some(&json!(format!(
            "https://cloud.test/cloud/acme/approve/{PROPOSAL}"
        )))
    );
}

#[test]
fn a_keyed_up_that_cloud_answers_with_a_link_returns_it_without_waiting() {
    let config = tempfile::tempdir().unwrap();
    let config = config.path().join("config.toml");
    crate::device::key::store(
        &crate::device::key::path(&config),
        &crate::device::key::generate(),
    )
    .unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("index.html"), "hello").unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let cloud = fake_cloud(move |route, _| {
        if route.starts_with("POST /api/config/upload/") {
            return (200, json!({}));
        }
        if route.starts_with("GET /api/cli/proposals/") {
            counted.fetch_add(1, Ordering::SeqCst);
            return (
                200,
                json!({ "proposal": { "status": "denied", "reason": null } }),
            );
        }
        let StoreCallError::Refused(refused) = link_answer("https://cloud.test") else {
            unreachable!()
        };
        (409, json!({ "error": refused }))
    });
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "--ployz-config", config.to_str().unwrap(), "up"])
        .unwrap();
    let matches = crate::cli::leaf_matches(&root);
    let store = cloud_store(&cloud, matches);
    let request = super::super::Request {
        environment: ployz_store::EnvironmentRef::default(),
        services: Vec::new(),
        version: None,
        source: Some(source.path().to_owned()),
        accept: Vec::new(),
        message: None,
        no_prune: false,
    };
    let Err(failure) = super::super::upload_and_ship(matches, &store, request, None) else {
        panic!("a link admitted a Deployment");
    };
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "a keyed CLI followed the link"
    );
    let details = failure.json().get("details").cloned().unwrap_or_default();
    assert_eq!(details.get("proposal_id"), Some(&json!(PROPOSAL)));
}

#[test]
fn only_cloud_s_link_answer_is_followed() {
    let plain = StoreCallError::Refused(RpcError::new(
        RpcErrorCode::Conflict,
        "A different deploy already asked for approval under this ID. Nothing deployed.",
    ));
    assert!(Linked::of(&plain).is_none());
    assert!(Linked::of(&link_answer("https://cloud.test")).is_some());
}

/// Cloud's own refusal envelopes for a failed query and an outage are tried again,
/// not taken as the end of the wait.
#[test]
fn cloud_failing_inside_or_unavailable_is_tried_again() {
    for code in ["INTERNAL", "UNAVAILABLE"] {
        let body = json!({"_tag":"PublicError", "code":code, "message":"try again"}).to_string();
        let refused = crate::cloud::reply::public_error(body.as_bytes()).unwrap();
        assert!(temporary(&StoreCallError::Refused(refused)), "{code}");
    }
    let body = json!({"_tag":"PublicError", "code":"NOT_FOUND", "message":"gone"}).to_string();
    let refused = crate::cloud::reply::public_error(body.as_bytes()).unwrap();
    assert!(!temporary(&StoreCallError::Refused(refused)));
}
