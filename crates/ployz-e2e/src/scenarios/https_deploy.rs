use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::acme::{Hostname, RevocationFreshness};
use ployz::deploy::certificate_unusable_reason;
use ployz::domain::DomainStatus;
use ployz::error::{DeployFailure, ServingFailure};
use ployz::machine::MachineId;
use ployz::runtime::{ParticipantReceipt, RuntimeActivationOutcome, RuntimeRevision, WorkloadId};
use ployz::serving::{RouteId, ServingActivationObservation, ServingGeneration, ServingTarget};

use super::deploy_fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn https_deploy_ensures_cert_commits_serving_and_verifies_activation() {
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let deploy = DeployFixtureBuilder::new()
        .contexts(contexts.clone())
        .build();

    let outcome = deploy
        .deploy_https(&deploy_context(), request())
        .await
        .expect("deploy success");

    assert_eq!(outcome.domain.domain().as_str(), "app.example.com");
    assert_eq!(outcome.runtime.revision, RuntimeRevision::new(3));
    assert_eq!(outcome.serving.generation(), ServingGeneration::new(11));
    assert_eq!(contexts.borrow().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn certificate_usability_reasons_keep_unknown_distinct() {
    let binding = request().manifest.https;
    let mut cases = Vec::new();
    let mut unknown = usable_certificate();
    unknown.revocation = RevocationFreshness::Unknown;
    cases.push((
        unknown.clone(),
        ployz::acme::CertificateUnusableReason::FreshnessUnknown,
    ));
    let mut revoked = usable_certificate();
    revoked.revocation = RevocationFreshness::KnownRevoked;
    cases.push((
        revoked.clone(),
        ployz::acme::CertificateUnusableReason::KnownRevoked,
    ));
    let mut short_lived = usable_certificate();
    short_lived.not_after = UNIX_EPOCH + Duration::from_secs(30);
    cases.push((
        short_lived.clone(),
        ployz::acme::CertificateUnusableReason::SafetyWindowTooShort,
    ));

    for (certificate, reason) in cases {
        assert_eq!(
            certificate_unusable_reason(
                &certificate,
                &binding,
                UNIX_EPOCH + Duration::from_secs(3_600)
            ),
            Some(reason)
        );
    }

    for certificate in [unknown, revoked, short_lived] {
        let deploy = DeployFixtureBuilder::new().certificate(certificate).build();
        assert_eq!(
            deploy.deploy_https(&deploy_context(), request()).await,
            Err(DeployFailure::CertificateUnusable)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn serving_commit_without_activation_is_not_success() {
    let deploy = DeployFixtureBuilder::new()
        .activation(ServingActivationObservation::Unknown)
        .build();

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()).await,
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn serving_activation_observer_must_see_commit_before_success() {
    let serving_commits = Rc::new(RefCell::new(0));
    let deploy = DeployFixtureBuilder::new()
        .serving_commits(serving_commits.clone())
        .lagging_serving_observer()
        .build();

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()).await,
        Err(DeployFailure::ServingFailed(ServingFailure::SnapshotStale))
    );
    assert_eq!(*serving_commits.borrow(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn retry_after_missing_serving_activation_observes_current_state() {
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let runtime = runtime_for(receipt());
    let serving_commits = Rc::new(RefCell::new(0));
    let serving_commit_state = FakeServingCommitState::default();
    let first = DeployFixtureBuilder::new()
        .activation(ServingActivationObservation::Unknown)
        .status(status.clone())
        .runtime(runtime.clone())
        .serving_commits(serving_commits.clone())
        .serving_commit_state(serving_commit_state.clone())
        .build();
    assert_eq!(
        first.deploy_https(&deploy_context(), request()).await,
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );

    let contexts = Rc::new(RefCell::new(Vec::new()));
    let deploy = DeployFixtureBuilder::new()
        .contexts(contexts.clone())
        .status(status)
        .runtime(runtime.clone())
        .serving_commits(serving_commits.clone())
        .serving_commit_state(serving_commit_state)
        .build();

    let outcome = deploy
        .deploy_https(&deploy_context(), request())
        .await
        .expect("retry succeeds from observation");
    assert_eq!(outcome.serving.generation(), ServingGeneration::new(11));
    assert!(contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*serving_commits.borrow(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn second_deploy_observes_current_state_without_mutation() {
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let runtime = runtime_for(receipt());
    let serving_commits = Rc::new(RefCell::new(0));
    let deploy = DeployFixtureBuilder::new()
        .contexts(contexts.clone())
        .status(status)
        .runtime(runtime.clone())
        .serving_commits(serving_commits.clone())
        .build();

    let _first = deploy
        .deploy_https(&deploy_context(), request())
        .await
        .expect("initial deploy");
    contexts.borrow_mut().clear();

    let second = deploy
        .deploy_https(&deploy_context(), request())
        .await
        .expect("second deploy");

    assert_eq!(second.runtime.revision, RuntimeRevision::new(3));
    assert!(contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*runtime.verifications.borrow(), 3);
    assert_eq!(*serving_commits.borrow(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn same_generation_serving_update_recommits_and_waits_for_activation() {
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let state = SharedDeployState::new();
    let deploy = DeployFixtureBuilder::new()
        .contexts(contexts.clone())
        .state(state.clone())
        .build();
    deploy
        .deploy_https(&deploy_context(), request())
        .await
        .expect("initial deploy");

    let mut conflicting = request();
    conflicting.manifest.serving_target =
        ServingTarget::parse("gateway-b").expect("conflicting target");

    let retry = DeployFixtureBuilder::new()
        .activation(activation_for(&conflicting))
        .contexts(contexts)
        .state(state.clone())
        .build();
    retry
        .deploy_https(&deploy_context(), conflicting)
        .await
        .expect("updated deploy");
    assert_eq!(*state.serving_commits().borrow(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn observed_ready_domain_repairs_missing_runtime_participant() {
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let first = DeployFixtureBuilder::new().status(status.clone()).build();
    first
        .deploy_https(&deploy_context(), request())
        .await
        .expect("initial deploy");
    let activations = Rc::new(RefCell::new(0));

    let retry = DeployFixtureBuilder::new()
        .status(status)
        .runtime(runtime_with_activation_visibility(
            receipt(),
            true,
            activations.clone(),
            Rc::new(RefCell::new(0)),
        ))
        .build();

    let outcome = retry
        .deploy_https(&deploy_context(), request())
        .await
        .expect("missing runtime is repaired");
    assert_eq!(outcome.runtime.revision, RuntimeRevision::new(3));
    assert_eq!(*activations.borrow(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_activation_must_be_observed_before_deploy_succeeds() {
    let activations = Rc::new(RefCell::new(0));
    let verifications = Rc::new(RefCell::new(0));
    let serving_commits = Rc::new(RefCell::new(0));
    let deploy = DeployFixtureBuilder::new()
        .runtime(runtime_with_activation_visibility(
            receipt(),
            false,
            activations.clone(),
            verifications.clone(),
        ))
        .serving_commits(serving_commits.clone())
        .build();

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()).await,
        Err(DeployFailure::RuntimeParticipantFailed)
    );
    assert_eq!(*activations.borrow(), 1);
    assert_eq!(*verifications.borrow(), 2);
    assert_eq!(*serving_commits.borrow(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_receipt_must_match_requested_participant() {
    let deploy = DeployFixtureBuilder::new()
        .runtime_outcome(RuntimeActivationOutcome::Activated(ParticipantReceipt {
            workload: WorkloadId::parse("other-workload").expect("workload"),
            machine: MachineId::parse("machine-a").expect("machine"),
            revision: RuntimeRevision::new(3),
        }))
        .build();

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()).await,
        Err(DeployFailure::RuntimeParticipantFailed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn serving_activation_must_match_committed_route_identity() {
    let deploy = DeployFixtureBuilder::new()
        .activation(ServingActivationObservation::Acknowledged {
            route: RouteId::parse("other-route").expect("route"),
            hostname: Hostname::parse("app.example.com").expect("hostname"),
            target: ServingTarget::parse("gateway-a").expect("target"),
            generation: ServingGeneration::new(11),
        })
        .build();

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()).await,
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );
}
