use std::cell::RefCell;
use std::rc::Rc;

use ployz::error::DeployFailure;
use ployz::serving::{ServingActivationStatus, ServingCheckpoint, ServingGeneration};

use super::https_deploy::{FakeOperations, engine, request, usable_certificate};

#[test]
fn retry_after_serving_checkpoint_still_requires_activation_proof() {
    let operations = FakeOperations::default();
    let first_attempt = engine(
        usable_certificate(),
        ServingActivationStatus::Unknown,
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        first_attempt.deploy_https(request()),
        Err(DeployFailure::ServingActivationFailed)
    );
    assert!(operations.terminal.borrow().is_empty());

    let second_attempt = engine(
        usable_certificate(),
        ServingActivationStatus::Acknowledged(ServingCheckpoint {
            generation: ServingGeneration::new(11),
        }),
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    second_attempt
        .deploy_https(request())
        .expect("retry verifies activation before success");
    assert_eq!(operations.terminal.borrow().len(), 1);
}
