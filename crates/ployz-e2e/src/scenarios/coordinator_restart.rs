use std::cell::RefCell;
use std::rc::Rc;

use ployz::error::DeployFailure;
use ployz::error::ServingFailure;
use ployz::serving::ServingActivationObservation;

use super::deploy_fixture::{
    DeployFixtureBuilder, SharedDeployState, activation_for, deploy_context, receipt, request,
};

#[tokio::test(flavor = "current_thread")]
async fn retry_after_missing_serving_activation_observes_activation_proof() {
    let state = SharedDeployState::new();
    let runtime = state.runtime_for(receipt());
    let first_deploy = DeployFixtureBuilder::new()
        .activation(ServingActivationObservation::Unknown)
        .state(state.clone())
        .runtime(runtime.clone())
        .build();

    assert_eq!(
        first_deploy
            .deploy_https(&deploy_context(), request())
            .await,
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );

    let retry_contexts = Rc::new(RefCell::new(Vec::new()));
    let retry_deploy = DeployFixtureBuilder::new()
        .contexts(retry_contexts.clone())
        .state(state.clone())
        .runtime(runtime.clone())
        .activation(activation_for(&request()))
        .build();

    retry_deploy
        .deploy_https(&deploy_context(), request())
        .await
        .expect("retry observes activation");
    assert!(retry_contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*state.serving_commits().borrow(), 1);
}
