#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! An agent's view of Deployments a push held for Wait for CI: a hold never stands in
//! for the Environment's latest Deployment.

use ployz_agent_tools::{
    Ask, Call, Place, Refusal, Step, StoreAt, ToolResult, Write, resume, start,
};
use ployz_core::RpcErrorCode;
use ployz_store::{
    Actor, Automated, ConfigStore, DeploymentQuery, DeploymentStatus, DeploymentSummary,
    ProjectName, Trusted,
};
use serde_json::{Value, json};

use ployz_store::testing::backend;
use ployz_store::testing::shop::*;

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

fn refused(result: ToolResult) -> Refusal {
    match result {
        ToolResult::Ok(value) => panic!("not refused: {value}"),
        ToolResult::Refused(refusal) => *refusal,
    }
}

/// An Environment whose only Deployment is a hold has no latest Deployment to show,
/// cancel, or start by default.
#[test]
fn an_environment_with_only_a_hold_has_no_deployment_yet() {
    let (store, who, _) = waiting();
    let shown = refused(tool(&store, &who, "deployment_show", json!({})));
    assert_eq!(shown.code, RpcErrorCode::NotFound);
    assert_eq!(shown.message, "shop/production has no Deployment yet");
    assert_eq!(shown.next, [Call::new("deploy", json!({}))]);
}

/// A Deployment number the Environment never had names the list of its
/// Deployments, in the same Project and Environment.
#[test]
fn a_missing_deployment_number_names_the_scoped_list() {
    let (store, who, _) = waiting();
    let shown = refused(tool(
        &store,
        &who,
        "deployment_show",
        json!({ "deployment": 999 }),
    ));
    assert_eq!(shown.code, RpcErrorCode::NotFound);
    assert_eq!(shown.message, "shop/production has no Deployment #999");
    assert_eq!(
        shown.next,
        [Call::new(
            "deployment_list",
            json!({ "project": "shop", "environment": "production" })
        )]
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
