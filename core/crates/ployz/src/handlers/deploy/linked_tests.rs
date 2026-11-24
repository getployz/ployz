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
                recreate: Vec::new(),
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
    let cloud = fake_cloud(|route, _| match route {
        route if route == format!("GET /api/cli/proposals/{PROPOSAL}") => (
            200,
            json!({ "proposal": { "status": "denied", "reason": "Not during the sale" } }),
        ),
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
fn json_detach_or_a_signed_deploy_gets_cloud_s_answer_without_following_it() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let cloud = fake_cloud(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        (500, json!({}))
    });
    let matches = deploy_matches();
    let store = cloud_store(&cloud, &matches);
    let id = DeploymentId::parse(mint()).unwrap();
    for (json, detach, signed) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let approval = Approval::of(json, detach, signed);
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

#[tokio::test(start_paused = true)]
async fn a_waiting_link_is_followed_until_its_deployment_is_admitted() {
    let (deployment, _) = admitted_view();
    let mut looks = 0;
    let stand = settled(WAIT, async || {
        looks += 1;
        Ok(if looks < 3 {
            Stand::Waiting
        } else {
            Stand::Admitted(Box::new(deployment.clone()))
        })
    })
    .await
    .unwrap();
    assert!(matches!(stand, Stand::Admitted(admitted) if admitted.id == deployment.id));
    assert_eq!(looks, 3);
}

#[tokio::test(start_paused = true)]
async fn nobody_approving_ends_the_wait_with_nothing_deployed_yet() {
    let started = tokio::time::Instant::now();
    let stand = settled(WAIT, async || Ok(Stand::Waiting)).await.unwrap();
    assert!(matches!(stand, Stand::Waiting));
    let waited = started.elapsed();
    assert!(waited >= WAIT && waited <= WAIT + FOLLOW_POLL, "{waited:?}");
    let linked = Linked::of(&link_answer("https://cloud.test")).unwrap();
    let StoreCallError::Stopped(failure) = linked.not_yet(Error::conflict(linked.waiting())) else {
        unreachable!()
    };
    assert_eq!(
        failure.to_string(),
        format!(
            "Nothing deployed yet. It waits for approval at https://cloud.test/cloud/acme/approve/{PROPOSAL}."
        )
    );
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
