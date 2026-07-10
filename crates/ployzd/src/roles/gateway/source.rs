//! Gateway projection source adapters.

use crate::fact_cache::FactCache;
use crate::intent::service::{IntentReadError, NatsIntentReader};
use crate::roles::gateway::projection::{
    GatewayProjectionError, GatewayProjectionInput, GatewayProjectionUpdate, GatewayRoute,
    GatewayServingEntry,
};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};

mod certificate_store;

pub use certificate_store::{GatewayCertificateStore, GatewayCertificateStoreError};

pub async fn load_gateway_projection_update_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &FactCache,
    certificate_store: &GatewayCertificateStore,
    now_seconds: u64,
) -> GatewayProjectionUpdate {
    match load_gateway_projection_input_from_nats(
        intent_reader,
        facts,
        certificate_store,
        now_seconds,
    )
    .await
    {
        Ok(input) => GatewayProjectionUpdate::SourceAvailable(input),
        Err(GatewaySourceError::Invalid { message }) => {
            GatewayProjectionUpdate::SourceInvalid(GatewayProjectionError::InvalidSource {
                message,
            })
        }
        Err(GatewaySourceError::Unavailable { message }) => {
            GatewayProjectionUpdate::SourceUnavailable(GatewayProjectionError::SourceUnavailable {
                message,
            })
        }
    }
}

pub async fn load_gateway_projection_input_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &FactCache,
    certificate_store: &GatewayCertificateStore,
    now_seconds: u64,
) -> Result<GatewayProjectionInput, GatewaySourceError> {
    let intent = async {
        intent_reader
            .intent()
            .await
            .map_err(GatewaySourceError::from)
    };
    let intent = intent.await?;
    let observed_machines = facts.machine_container_snapshots();

    let custom_cert_bundles = load_routed_custom_certificates(
        &intent.custom_certificates,
        &intent.route_bindings,
        certificate_store,
        now_seconds,
    )
    .map_err(|error| GatewaySourceError::Invalid {
        message: error.to_string(),
    })?;

    Ok(gateway_projection_input_from_state(
        match intent.managed_lease {
            ployz_core::state::ManagedLeaseProjection::Ready { bundle, .. } => Some(bundle),
            ployz_core::state::ManagedLeaseProjection::Unacquired
            | ployz_core::state::ManagedLeaseProjection::RecordOnly { .. } => None,
        },
        custom_cert_bundles,
        intent.acme_http01_challenges,
        intent.route_bindings,
        intent.serving_target_entries,
        observed_machines,
    ))
}

fn load_routed_custom_certificates(
    active_certificates: &[ployz_core::cert::ActiveCertState],
    routes: &[RouteBindingState],
    certificate_store: &GatewayCertificateStore,
    now_seconds: u64,
) -> Result<Vec<ployz_core::cert::CustomCertBundle>, GatewayCertificateStoreError> {
    active_certificates
        .iter()
        .filter(|active| {
            routes.iter().any(|route| {
                route.target.port.get() == 443 && route.target.hostname == active.hostname
            })
        })
        .map(|active| certificate_store.load_at(active, now_seconds))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewaySourceError {
    #[error("invalid gateway source: {message}")]
    Invalid { message: String },
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
    managed_cert_bundle: Option<ployz_core::cert::ManagedCertBundle>,
    custom_cert_bundles: Vec<ployz_core::cert::CustomCertBundle>,
    challenges: Vec<ployz_core::cert::AcmeHttp01Challenge>,
    routes: Vec<RouteBindingState>,
    serving: Vec<ServingTargetEntry>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        managed_cert_bundle,
        custom_cert_bundles,
        challenges,
        routes: routes.into_iter().map(gateway_route_from_state).collect(),
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

fn gateway_route_from_state(state: RouteBindingState) -> GatewayRoute {
    GatewayRoute {
        target: state.target,
        endpoint_port: state.endpoint_port,
        namespace_id: state.namespace_id,
        service_id: state.service_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::cert::{ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow};
    use ployz_core::ids::CertId;
    use ployz_core::ops::RouteTarget;
    use ployz_test_support::ids::{namespace_id, route_hostname, route_port, service_id};

    #[test]
    fn unrouted_certificate_without_local_material_does_not_block_projection_source() {
        let state = tempfile::tempdir().expect("gateway state");

        assert!(
            load_routed_custom_certificates(
                &[active_certificate("app.example.com")],
                &[],
                &GatewayCertificateStore::new(state.path().to_path_buf()),
                1_500,
            )
            .expect("unrouted metadata is ignored")
            .is_empty()
        );
    }

    #[test]
    fn routed_certificate_without_local_material_invalidates_projection_source() {
        let state = tempfile::tempdir().expect("gateway state");

        assert!(matches!(
            load_routed_custom_certificates(
                &[active_certificate("app.example.com")],
                &[route("app.example.com", 443)],
                &GatewayCertificateStore::new(state.path().to_path_buf()),
                1_500,
            ),
            Err(GatewayCertificateStoreError::ArtifactFile { .. })
        ));
    }

    #[test]
    fn routed_certificate_with_invalid_local_material_invalidates_projection_source() {
        let state = tempfile::tempdir().expect("gateway state");
        let store = GatewayCertificateStore::new(state.path().to_path_buf());
        let active = active_certificate("app.example.com");
        let artifact = store.artifact_path(&active).expect("artifact path");
        std::fs::create_dir_all(artifact.parent().expect("artifact directory"))
            .expect("create artifact directory");
        std::fs::write(artifact, b"invalid material").expect("write invalid artifact");

        assert!(matches!(
            load_routed_custom_certificates(
                &[active],
                &[route("app.example.com", 443)],
                &store,
                1_500,
            ),
            Err(GatewayCertificateStoreError::InvalidMaterial { .. })
        ));
    }

    fn active_certificate(hostname: &str) -> ActiveCertState {
        ActiveCertState {
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
        }
    }

    fn route(hostname: &str, port: u16) -> RouteBindingState {
        RouteBindingState {
            namespace_id: namespace_id("default"),
            target: RouteTarget::new(route_hostname(hostname), route_port(port)),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
        }
    }
}
