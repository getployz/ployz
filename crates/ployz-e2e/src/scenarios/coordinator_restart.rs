use std::cell::RefCell;
use std::rc::Rc;

use ployz::error::DeployFailure;
use ployz::error::ServingFailure;
use ployz::serving::ServingActivationObservation;

use super::https_deploy::{
    SharedDeployState, activation_for, deploy_context, engine_with_state, receipt, request,
    usable_certificate,
};

#[test]
fn retry_after_missing_serving_activation_observes_activation_proof() {
    let state = SharedDeployState::new();
    let runtime = state.runtime_for(receipt());
    let first_deploy = engine_with_state(
        usable_certificate(),
        ServingActivationObservation::Unknown,
        Rc::new(RefCell::new(Vec::new())),
        state.clone(),
        runtime.clone(),
    );

    assert_eq!(
        first_deploy.deploy_https(&deploy_context(), request()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );

    let retry_contexts = Rc::new(RefCell::new(Vec::new()));
    let retry_deploy = engine_with_state(
        usable_certificate(),
        activation_for(&request()),
        retry_contexts.clone(),
        state.clone(),
        runtime.clone(),
    );

    retry_deploy
        .deploy_https(&deploy_context(), request())
        .expect("retry observes activation");
    assert!(retry_contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*state.serving_commits().borrow(), 1);
}
