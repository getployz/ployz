use std::cell::RefCell;
use std::rc::Rc;

use ployz::error::DeployFailure;
use ployz::operation::TerminalMarker;
use ployz::serving::{ServingActivationObservation, ServingGeneration};

use super::https_deploy::{FakeOperations, command, engine, request, usable_certificate};

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
        first_attempt.deploy_https(command(), request()),
        Err(DeployFailure::ServingActivationFailed)
    );
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [TerminalMarker::Failed(Vec::new())]
    );

    let second_attempt = engine(
        usable_certificate(),
        ServingActivationObservation::Acknowledged {
            generation: ServingGeneration::new(11),
        },
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    second_attempt
        .deploy_https(command(), request())
        .expect("retry verifies activation before success");
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [
            TerminalMarker::Failed(Vec::new()),
            TerminalMarker::Succeeded
        ]
    );
}
