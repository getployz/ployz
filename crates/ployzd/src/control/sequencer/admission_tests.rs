use super::*;
use crate::control::intent::ingress_intent::IngressIntentStore;
use crate::control::store::CoreStore;
use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployRoute, DeployRouteTarget, DeployServiceSpec, ImageReference,
    ImageSource, ReplicaCount,
};
use ployz_core::ingress::{
    AutomaticHostnameConfiguration, AutomaticHostnameLabel, IngressConfiguration,
    PloyzDnsTargetIntent,
};
use ployz_core::ops::RoutePort;
use ployz_test_support::ids::{idempotency_key, namespace_id, operation_id, service_id};

#[test]
fn deploy_reservation_expires_at_its_deadline() {
    let namespace_id = NamespaceId::try_new("default").expect("valid namespace id");
    let reservation_id = DeployReservationId::first();
    let expired_at = DeployReservationExpiresAt::try_new(10).expect("valid expiration");
    let mut reservations = BTreeMap::from([(
        namespace_id.clone(),
        BTreeMap::from([(reservation_id, expired_at)]),
    )]);

    let error = validate_deploy_reservation_at(
        &mut reservations,
        &namespace_id,
        reservation_id,
        expired_at.unix_seconds(),
    )
    .expect_err("reservation expires at its deadline");

    assert!(matches!(
        error,
        SubmitCommandError::ReservationExpired {
            namespace_id: expired_namespace,
            reservation_id: expired_reservation,
            expired_at: deadline,
        } if expired_namespace == namespace_id
            && expired_reservation == reservation_id
            && deadline == expired_at
    ));
}

#[tokio::test]
async fn duplicate_deploy_submission_keeps_ingress_fence_owned_by_original() {
    let (_nats, controllers, intent) = test_controllers().await;
    intent
        .replace(test_ingress_configuration())
        .await
        .expect("ingress configuration stores");
    let reservation_id = controllers
        .reserve_deploy(&namespace_id("default"))
        .await
        .expect("deploy reservation issues")
        .reservation_id;
    let command = DeploySubmitCommand {
        operation_id: operation_id("op_original"),
        idempotency_key: idempotency_key("idem_original"),
        reservation_id,
        target: automatic_deploy_request(),
        registry_credentials: BTreeMap::new(),
    };

    controllers
        .submit_deploy(command.clone())
        .await
        .expect("original deploy submits");
    controllers
        .submit_deploy(command)
        .await
        .expect("duplicate deploy submits idempotently");
    let conflicting_reservation = controllers
        .reserve_deploy(&namespace_id("default"))
        .await
        .expect("conflicting deploy reservation issues")
        .reservation_id;
    let error = controllers
        .submit_deploy(DeploySubmitCommand {
            operation_id: operation_id("op_conflicting"),
            idempotency_key: idempotency_key("idem_conflicting"),
            reservation_id: conflicting_reservation,
            target: automatic_deploy_request(),
            registry_credentials: BTreeMap::new(),
        })
        .await
        .expect_err("conflicting deploy remains fenced");

    assert!(matches!(
        error,
        SubmitCommandError::IngressBusy { owner, .. }
            if owner == operation_id("op_original")
    ));
}

#[tokio::test]
async fn duplicate_ingress_configure_submission_keeps_fence_owned_by_original() {
    let (_nats, controllers, intent) = test_controllers().await;
    let command = IngressConfigureSubmitCommand {
        operation_id: operation_id("op_original"),
        configuration: test_ingress_configuration(),
    };

    controllers
        .submit_ingress_configure(command.clone(), &intent)
        .await
        .expect("original ingress configure submits");
    controllers
        .submit_ingress_configure(command, &intent)
        .await
        .expect("duplicate ingress configure submits idempotently");
    let error = controllers
        .submit_ingress_configure(
            IngressConfigureSubmitCommand {
                operation_id: operation_id("op_conflicting"),
                configuration: test_ingress_configuration(),
            },
            &intent,
        )
        .await
        .expect_err("conflicting ingress configure remains fenced");

    assert!(matches!(
        error,
        IngressConfigureSubmitError::Busy { owner }
            if owner == operation_id("op_original")
    ));
}

async fn test_controllers() -> (
    ployz_test_support::nats::TestNats,
    OperationControllers,
    IngressIntentStore,
) {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store opens");
    let intent = IngressIntentStore::new(core_store.clone());
    let controllers = OperationControllers::new(
        OperationRepository::open(core_store, nats.controller.clone()),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(crate::config::DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    );
    (nats, controllers, intent)
}

fn test_ingress_configuration() -> IngressConfiguration {
    IngressConfiguration::try_new(
        AutomaticHostnameConfiguration::custom("apps.example.com")
            .expect("automatic hostname suffix is valid"),
        PloyzDnsTargetIntent::Disabled,
    )
    .expect("valid ingress configuration")
}

fn automatic_deploy_request() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("registry.example/api:latest")
                .expect("image reference is valid"),
            image_source: ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("replica count is valid"),
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: vec![DeployRoute {
                target: DeployRouteTarget::AutoHostname {
                    label: AutomaticHostnameLabel::try_new("api")
                        .expect("automatic hostname label is valid"),
                },
                endpoint_port: RoutePort::try_new(8080).expect("route port is valid"),
            }],
        }],
    }
}
