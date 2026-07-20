//! Bounded local Dockerfile and Railpack image build effects.

mod lifecycle;
mod logs;
mod oci;
mod plan;
mod runner;
mod source;
mod workspace;

pub use logs::{BuildLogDestination, BuildLogProgress};
pub use oci::ValidatedOciLayout;
pub use runner::{
    BuildExecutionError, BuildExecutionRequest, BuildExecutionResult, DockerBuildExecutor,
    DockerHubRegistryMirror, DockerHubRegistryMirrorError,
};

pub fn native_oci_platform() -> Result<ployz_core::image::OciPlatform, String> {
    oci_platform(std::env::consts::OS, std::env::consts::ARCH)
}

pub async fn probe_docker_runtime() -> Result<ployz_core::image::OciPlatform, BuildExecutionError> {
    let native = native_oci_platform().map_err(|message| BuildExecutionError::Infrastructure {
        action: "normalize native build platform",
        message,
        log_summary: ployz_core::build::BuildLogSummary::none(),
    })?;
    lifecycle::probe_docker_runtime(&native).await
}

pub async fn railpack_helper_is_ready(platform: &ployz_core::image::OciPlatform) -> bool {
    let Ok(adapter_toolchain) = plan::railpack_toolchain_for_platform(platform) else {
        return false;
    };
    workspace::verify_helper(&adapter_toolchain).await.is_ok()
}

fn oci_platform(os: &str, architecture: &str) -> Result<ployz_core::image::OciPlatform, String> {
    let architecture = match architecture {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        architecture => {
            return Err(format!(
                "unsupported build machine architecture {architecture}"
            ));
        }
    };
    ployz_core::image::OciPlatform::try_new(os, architecture).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_architectures_use_oci_names() {
        assert_eq!(
            oci_platform("linux", "x86_64")
                .expect("amd64")
                .architecture(),
            "amd64"
        );
        assert_eq!(
            oci_platform("linux", "aarch64")
                .expect("arm64")
                .architecture(),
            "arm64"
        );
        assert!(oci_platform("linux", "riscv64").is_err());
    }
}
