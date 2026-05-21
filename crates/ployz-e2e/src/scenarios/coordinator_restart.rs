use std::cell::RefCell;
use std::rc::Rc;

use ployz::error::DeployFailure;
use ployz::error::ServingFailure;
use ployz::serving::ServingActivationObservation;

use super::https_deploy::{
    FakeOperations, activation_for, command, engine, request, usable_certificate,
};

#[test]
fn retry_after_serving_checkpoint_still_requires_activation_proof() {
    let operations = FakeOperations::default();
    let first_attempt = engine(
        usable_certificate(),
        ServingActivationObservation::Unknown,
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        first_attempt.deploy_https(command()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [polis::TerminalMarker::Interrupted]
    );

    let second_attempt = engine(
        usable_certificate(),
        activation_for(&request()),
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        second_attempt.deploy_https(command()),
        Err(DeployFailure::Interrupted)
    );
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [polis::TerminalMarker::Interrupted]
    );
}
