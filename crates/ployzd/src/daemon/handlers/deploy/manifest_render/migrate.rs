use ployz_api::MigrateServiceRequest;
pub(in crate::daemon::handlers::deploy) use ployz_orchestrator::deploy::{
    MigrateRenderError, encode_migrate_manifest_json,
};
use ployz_orchestrator::deploy::{
    MigrateServiceManifestRequest, render_migrate_service_manifest as render_migrate_manifest,
    validate_migrate_service_manifest_request,
};
use ployz_store_api::StoreDriver;

fn migrate_manifest_request(request: &MigrateServiceRequest) -> MigrateServiceManifestRequest {
    MigrateServiceManifestRequest {
        namespace: request.namespace.clone(),
        service: request.service.clone(),
        target_machine: request.target_machine.clone(),
    }
}

pub(in crate::daemon::handlers::deploy) fn migrate_render_error_code(
    error: &MigrateRenderError,
) -> &'static str {
    match error {
        MigrateRenderError::InvalidRequest { .. } => "MIGRATE_INVALID_REQUEST",
        MigrateRenderError::ManifestEncodeFailed { .. } => "MIGRATE_MANIFEST_ENCODE_FAILED",
        MigrateRenderError::ExportFailed { .. } => "MIGRATE_EXPORT_FAILED",
        MigrateRenderError::EmptyTargetMachine
        | MigrateRenderError::ServiceMissing { .. }
        | MigrateRenderError::NoManagedVolumeMounts { .. }
        | MigrateRenderError::UnsupportedBindMount { .. }
        | MigrateRenderError::DuplicateManagedVolumeMount { .. }
        | MigrateRenderError::MissingCommittedVolume { .. }
        | MigrateRenderError::AlreadyOnTarget { .. }
        | MigrateRenderError::UnsupportedVolumeScope { .. } => "MIGRATE_RENDER_FAILED",
    }
}

pub(in crate::daemon::handlers::deploy) fn validate_migrate_service_request(
    request: &MigrateServiceRequest,
) -> Result<(), MigrateRenderError> {
    validate_migrate_service_manifest_request(&migrate_manifest_request(request))
}

pub(in crate::daemon::handlers::deploy) async fn render_migrate_service_manifest(
    store: &StoreDriver,
    request: &MigrateServiceRequest,
) -> Result<ployz_spec::DeployManifest, MigrateRenderError> {
    render_migrate_manifest(store, &migrate_manifest_request(request)).await
}
