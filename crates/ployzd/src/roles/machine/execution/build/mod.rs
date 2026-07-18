//! Bounded machine-local image build effects.

mod lifecycle;
mod logs;
mod oci;
mod plan;
mod runner;
mod source;
mod workspace;

pub(crate) use logs::BuildLogProgress;
pub(crate) use oci::ValidatedOciLayout;
pub(crate) use runner::{
    BuildExecutionError, BuildExecutionRequest, BuildExecutionResult, DockerBuildExecutor,
};

pub(crate) async fn railpack_helper_is_ready(platform: &ployz_core::image::OciPlatform) -> bool {
    let Ok(adapter_toolchain) = plan::railpack_toolchain_for_platform(platform) else {
        return false;
    };
    workspace::verify_helper(&adapter_toolchain).await.is_ok()
}
