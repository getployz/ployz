use std::cell::RefCell;
use std::rc::Rc;

use ployz::domain::DomainStatus;
use ployz::error::DeployFailure;
use ployz::error::ServingFailure;
use ployz::serving::ServingActivationObservation;

use super::https_deploy::{
    activation_for, deploy_context, engine_with_shared_state, receipt, request, runtime_for,
    usable_certificate,
};

#[test]
fn retry_after_missing_serving_activation_observes_activation_proof() {
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let runtime = runtime_for(receipt());
    let serving_commits = Rc::new(RefCell::new(0));
    let first_deploy = engine_with_shared_state(
        usable_certificate(),
        ServingActivationObservation::Unknown,
        Rc::new(RefCell::new(Vec::new())),
        status.clone(),
        runtime.clone(),
        serving_commits.clone(),
    );

    assert_eq!(
        first_deploy.deploy_https(&deploy_context(), request()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );

    let retry_contexts = Rc::new(RefCell::new(Vec::new()));
    let retry_deploy = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        retry_contexts.clone(),
        status,
        runtime.clone(),
        serving_commits.clone(),
    );

    retry_deploy
        .deploy_https(&deploy_context(), request())
        .expect("retry observes activation");
    assert!(retry_contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*serving_commits.borrow(), 1);
}
