use ployz_api::{BranchNamespaceRequest, BranchResourceMode};
pub(in crate::daemon::handlers::deploy) use ployz_orchestrator::deploy::encode_branch_manifest_json;
use ployz_orchestrator::deploy::{
    BranchManifestResourceMode, BranchManifestResourceModeOverride, BranchNamespaceManifestRequest,
    BranchRenderError, render_branch_namespace_manifest as render_branch_manifest,
    validate_branch_namespace_manifest_request,
};
use ployz_store_api::StoreDriver;

fn branch_manifest_mode(mode: BranchResourceMode) -> BranchManifestResourceMode {
    match mode {
        BranchResourceMode::Fresh => BranchManifestResourceMode::Fresh,
        BranchResourceMode::Branch => BranchManifestResourceMode::Branch,
    }
}

fn branch_manifest_overrides(
    overrides: &[ployz_api::BranchResourceModeOverride],
) -> Vec<BranchManifestResourceModeOverride> {
    overrides
        .iter()
        .map(|override_| BranchManifestResourceModeOverride {
            name: override_.name.clone(),
            mode: branch_manifest_mode(override_.mode),
        })
        .collect()
}

fn branch_manifest_request(request: &BranchNamespaceRequest) -> BranchNamespaceManifestRequest {
    BranchNamespaceManifestRequest {
        source_namespace: request.source_namespace.clone(),
        target_namespace: request.target_namespace.clone(),
        default_service_mode: branch_manifest_mode(request.default_service_mode),
        default_volume_mode: branch_manifest_mode(request.default_volume_mode),
        services: branch_manifest_overrides(&request.services),
        volumes: branch_manifest_overrides(&request.volumes),
    }
}

pub(in crate::daemon::handlers::deploy) fn branch_render_error_code(
    error: &BranchRenderError,
) -> &'static str {
    match error {
        BranchRenderError::InvalidRequest { .. } => "BRANCH_INVALID_REQUEST",
        BranchRenderError::ExportFailed { .. } => "BRANCH_EXPORT_FAILED",
        BranchRenderError::ManifestEncodeFailed { .. } => "BRANCH_MANIFEST_ENCODE_FAILED",
        BranchRenderError::EmptySource { .. } => "BRANCH_SOURCE_EMPTY",
        BranchRenderError::UnknownService { .. } => "BRANCH_UNKNOWN_SERVICE",
        BranchRenderError::DuplicateService { .. } => "BRANCH_DUPLICATE_SERVICE",
        BranchRenderError::UnknownVolume { .. } => "BRANCH_UNKNOWN_VOLUME",
        BranchRenderError::DuplicateVolume { .. } => "BRANCH_DUPLICATE_VOLUME",
        BranchRenderError::InvalidManifest { .. } => "BRANCH_INVALID_MANIFEST",
    }
}

pub(in crate::daemon::handlers::deploy) fn validate_branch_namespace_request(
    request: &BranchNamespaceRequest,
) -> Result<(), BranchRenderError> {
    validate_branch_namespace_manifest_request(&branch_manifest_request(request))
}

pub(in crate::daemon::handlers::deploy) async fn render_branch_namespace_manifest(
    store: &StoreDriver,
    request: &BranchNamespaceRequest,
) -> Result<ployz_spec::DeployManifest, BranchRenderError> {
    render_branch_manifest(store, &branch_manifest_request(request)).await
}
