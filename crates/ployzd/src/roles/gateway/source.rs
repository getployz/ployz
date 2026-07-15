//! Gateway projection source adapters.

use crate::control::intent::service::{IntentReadError, NatsIntentReader};
use crate::role_testimony::RoleTestimonyCache;
use crate::roles::gateway::projection::{
    GatewayCertificateBundle, GatewayCertificateMaterialFailure,
    GatewayCertificateMaterialFailureKind, GatewayProjectionError, GatewayProjectionInput,
    GatewayProjectionUpdate, GatewayRoute, GatewayServingEntry,
};
use ployz_core::ingress::{
    ActiveCertificateMetadata, AutomaticHostnameConfiguration, CertificateOwner, RouteBindingOrigin,
};
use ployz_core::intent::RouteBindingState;
use ployz_core::intent::ServingTargetEntry;
use ployz_core::machine::runtime::MachineContainerObservationSnapshot;

mod certificate_store;

pub use certificate_store::{GatewayCertificateStore, GatewayCertificateStoreError};

pub async fn load_gateway_projection_update_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &RoleTestimonyCache,
    certificate_store: &GatewayCertificateStore,
) -> GatewayProjectionUpdate {
    match load_gateway_projection_input_from_nats(intent_reader, facts, certificate_store).await {
        Ok(input) => GatewayProjectionUpdate::SourceAvailable(Box::new(input)),
        Err(GatewaySourceError::Unavailable { message }) => {
            GatewayProjectionUpdate::SourceUnavailable(GatewayProjectionError::SourceUnavailable {
                message,
            })
        }
    }
}

pub async fn load_gateway_projection_input_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &RoleTestimonyCache,
    certificate_store: &GatewayCertificateStore,
) -> Result<GatewayProjectionInput, GatewaySourceError> {
    let intent = async {
        intent_reader
            .intent()
            .await
            .map_err(GatewaySourceError::from)
    };
    let intent = intent.await?;
    let observed_machines = facts.machine_container_snapshots();

    let (certificate_bundles, certificate_failures) = load_routed_certificates(
        &intent.active_certificates,
        &intent.route_bindings,
        &intent.automatic_hostname_configuration,
        certificate_store,
    );

    Ok(gateway_projection_input_from_state(
        certificate_bundles,
        certificate_failures,
        intent.route_bindings,
        intent.automatic_hostname_configuration,
        intent.serving_target_entries,
        observed_machines,
    ))
}

fn load_routed_certificates(
    active_certificates: &[ActiveCertificateMetadata],
    routes: &[RouteBindingState],
    automatic_hostnames: &AutomaticHostnameConfiguration,
    certificate_store: &GatewayCertificateStore,
) -> (
    Vec<GatewayCertificateBundle>,
    Vec<GatewayCertificateMaterialFailure>,
) {
    let mut bundles = Vec::new();
    let mut failures = Vec::new();
    for metadata in active_certificates
        .iter()
        .filter(|metadata| certificate_is_referenced(&metadata.owner, routes, automatic_hostnames))
    {
        match certificate_store.load_at(&metadata.active, current_unix_seconds()) {
            Ok(bundle) => bundles.push(GatewayCertificateBundle {
                owner: metadata.owner.clone(),
                bundle,
            }),
            Err(error) => {
                let kind = if matches!(error, GatewayCertificateStoreError::NotUsable { .. }) {
                    GatewayCertificateMaterialFailureKind::Unusable
                } else {
                    GatewayCertificateMaterialFailureKind::MissingOrInvalid
                };
                failures.push(GatewayCertificateMaterialFailure {
                    owner: metadata.owner.clone(),
                    kind,
                    message: error.to_string(),
                });
            }
        }
    }
    (bundles, failures)
}

fn certificate_is_referenced(
    owner: &CertificateOwner,
    routes: &[RouteBindingState],
    automatic_hostnames: &AutomaticHostnameConfiguration,
) -> bool {
    routes.iter().any(|route| match owner {
        CertificateOwner::PloyzAutomaticNamespace => {
            matches!(automatic_hostnames, AutomaticHostnameConfiguration::Ployz)
                && route.origin == RouteBindingOrigin::Automatic
        }
        CertificateOwner::RouteBinding { route_binding_id } => &route.id == route_binding_id,
    })
}

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewaySourceError {
    #[error("gateway source unavailable: {message}")]
    Unavailable { message: String },
}

impl From<IntentReadError> for GatewaySourceError {
    fn from(error: IntentReadError) -> Self {
        Self::Unavailable {
            message: error.to_string(),
        }
    }
}

fn gateway_projection_input_from_state(
    certificate_bundles: Vec<GatewayCertificateBundle>,
    certificate_failures: Vec<GatewayCertificateMaterialFailure>,
    routes: Vec<RouteBindingState>,
    automatic_hostnames: AutomaticHostnameConfiguration,
    serving: Vec<ServingTargetEntry>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        certificate_bundles,
        certificate_failures,
        challenges: Vec::new(),
        routes: routes
            .into_iter()
            .map(|state| gateway_route_from_state(state, &automatic_hostnames))
            .collect(),
        serving: serving
            .into_iter()
            .map(|state| GatewayServingEntry {
                namespace_id: state.namespace_id,
                service_id: state.service_id,
                namespace_revision_entry_id: state.namespace_revision_entry_id,
            })
            .collect(),
        observed_machines,
    }
}

fn gateway_route_from_state(
    state: RouteBindingState,
    automatic_hostnames: &AutomaticHostnameConfiguration,
) -> GatewayRoute {
    let certificate_owner = match (&state.origin, automatic_hostnames) {
        (RouteBindingOrigin::Automatic, AutomaticHostnameConfiguration::Ployz) => {
            CertificateOwner::PloyzAutomaticNamespace
        }
        (RouteBindingOrigin::Declared | RouteBindingOrigin::Automatic, _) => {
            CertificateOwner::RouteBinding {
                route_binding_id: state.id.clone(),
            }
        }
    };
    GatewayRoute {
        id: state.id,
        origin: state.origin,
        certificate_owner,
        target: state.target,
        endpoint_port: state.endpoint_port,
        namespace_id: state.namespace_id,
        service_id: state.service_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::certificate::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
    };
    use ployz_core::ids::{CertId, RouteBindingId};
    use ployz_core::ingress::{ActiveCertificateMetadata, CertificateOwner};
    use ployz_core::operation::RouteTarget;
    use ployz_test_support::ids::{namespace_id, route_hostname, route_port, service_id};

    #[test]
    fn unrouted_certificate_without_local_material_does_not_block_projection_source() {
        let state = tempfile::tempdir().expect("gateway state");

        assert!(
            load_routed_certificates(
                &[active_certificate("app.example.com")],
                &[],
                &AutomaticHostnameConfiguration::Disabled,
                &GatewayCertificateStore::new(state.path().to_path_buf()),
            )
            .0
            .is_empty()
        );
    }

    #[test]
    fn routed_certificate_without_local_material_is_reported_per_owner() {
        let state = tempfile::tempdir().expect("gateway state");

        assert!(matches!(
            load_routed_certificates(
                &[active_certificate("app.example.com")],
                &[route("app.example.com", 443)],
                &AutomaticHostnameConfiguration::Disabled,
                &GatewayCertificateStore::new(state.path().to_path_buf()),
            )
            .1
            .as_slice(),
            [GatewayCertificateMaterialFailure { owner, .. }]
                if owner == &route_owner()
        ));
    }

    #[test]
    fn routed_certificate_with_invalid_local_material_is_reported_per_owner() {
        let state = tempfile::tempdir().expect("gateway state");
        let store = GatewayCertificateStore::new(state.path().to_path_buf());
        let active = active_certificate("app.example.com");
        let artifact = store.artifact_path(&active.active).expect("artifact path");
        std::fs::create_dir_all(artifact.parent().expect("artifact directory"))
            .expect("create artifact directory");
        std::fs::write(artifact, b"invalid material").expect("write invalid artifact");

        assert!(matches!(
            load_routed_certificates(
                &[active],
                &[route("app.example.com", 443)],
                &AutomaticHostnameConfiguration::Disabled,
                &store,
            )
            .1
            .as_slice(),
            [GatewayCertificateMaterialFailure { owner, .. }]
                if owner == &route_owner()
        ));
    }

    fn active_certificate(hostname: &str) -> ActiveCertificateMetadata {
        ActiveCertificateMetadata {
            owner: route_owner(),
            active: ActiveCertState {
                cert_id: CertId::try_new("cert_app").expect("cert id"),
                hostname: route_hostname(hostname),
                bundle_ref: CertBundleRef::try_new(format!(
                    "sha256:{}:/core/owned.bundle",
                    "a".repeat(64)
                ))
                .expect("bundle ref"),
                validity: CertValidityWindow::try_new(
                    CertValidAt::try_new(1_000).expect("not before"),
                    CertValidAt::try_new(2_000).expect("not after"),
                )
                .expect("validity"),
            },
        }
    }

    fn route_owner() -> CertificateOwner {
        CertificateOwner::RouteBinding {
            route_binding_id: RouteBindingId::try_new("route_app").expect("route binding id"),
        }
    }

    fn route(hostname: &str, _port: u16) -> RouteBindingState {
        RouteBindingState {
            id: RouteBindingId::try_new("route_app").expect("route binding id"),
            namespace_id: namespace_id("default"),
            target: RouteTarget::new(route_hostname(hostname)),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            origin: RouteBindingOrigin::Declared,
        }
    }
}
