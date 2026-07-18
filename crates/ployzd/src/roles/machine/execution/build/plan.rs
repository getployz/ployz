use super::source::CheckedOutGitSource;
use ployz_core::build::{BuildAdapter, RailpackCacheKey, railpack_pins};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::install::{InstallArtifactVersion, InstallSha256Digest};
use ployz_core::operation::{BuildAdapterToolchainEvidence, BuildToolchainEvidence};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BuildToolchain {
    pub(super) buildkit_reference: String,
    pub(super) buildkit_manifest_digest: OciDigest,
    pub(super) adapter: BuildAdapterToolchain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BuildAdapterToolchain {
    Dockerfile,
    Railpack {
        helper_path: PathBuf,
        helper_version: InstallArtifactVersion,
        helper_sha256: InstallSha256Digest,
        frontend_reference: String,
        frontend_manifest_digest: OciDigest,
    },
}

impl BuildToolchain {
    #[must_use]
    pub(super) fn evidence(&self) -> BuildToolchainEvidence {
        BuildToolchainEvidence {
            buildkit_image: self.buildkit_manifest_digest.clone(),
            adapter: match &self.adapter {
                BuildAdapterToolchain::Dockerfile => BuildAdapterToolchainEvidence::Dockerfile,
                BuildAdapterToolchain::Railpack {
                    helper_path: _,
                    helper_version,
                    helper_sha256,
                    frontend_reference: _,
                    frontend_manifest_digest,
                } => BuildAdapterToolchainEvidence::Railpack {
                    helper_version: helper_version.clone(),
                    helper_sha256: helper_sha256.clone(),
                    frontend_image: frontend_manifest_digest.clone(),
                },
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BuildExecutionPlan {
    pub(super) prepare: Option<PrepareCommand>,
    pub(super) buildctl_arguments: Vec<String>,
    pub(super) oci_layout: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PrepareCommand {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

pub(super) fn toolchain_for_platform(
    platform: &OciPlatform,
    adapter: &BuildAdapter,
) -> Result<BuildToolchain, BuildPlanError> {
    let pins = railpack_pins().map_err(invalid_pin)?;
    let (buildkit_reference, buildkit_manifest_digest) = buildkit_for_platform(platform)?;
    let railpack = pins
        .for_architecture(platform.architecture())
        .ok_or_else(|| BuildPlanError::UnsupportedPlatform {
            platform: platform.clone(),
        })?;
    let frontend_manifest_digest = digest(railpack.frontend_digest())?;
    let adapter = match adapter {
        BuildAdapter::Dockerfile { .. } => BuildAdapterToolchain::Dockerfile,
        BuildAdapter::Railpack { .. } => BuildAdapterToolchain::Railpack {
            helper_path: PathBuf::from(pins.install_path()),
            helper_version: InstallArtifactVersion::try_new(pins.version())
                .map_err(|error| invalid_pin(error.to_string()))?,
            helper_sha256: InstallSha256Digest::try_new(railpack.binary_sha256())
                .map_err(|error| invalid_pin(error.to_string()))?,
            frontend_reference: format!(
                "{}@{}",
                pins.frontend_reference(),
                railpack.frontend_digest()
            ),
            frontend_manifest_digest,
        },
    };
    Ok(BuildToolchain {
        buildkit_reference,
        buildkit_manifest_digest,
        adapter,
    })
}

pub(super) fn buildkit_for_platform(
    platform: &OciPlatform,
) -> Result<(String, OciDigest), BuildPlanError> {
    if platform.os() != "linux" {
        return Err(BuildPlanError::UnsupportedPlatform {
            platform: platform.clone(),
        });
    }
    let buildkit = match platform.architecture() {
        "amd64" => "sha256:2caaaf9bc673a82d5b0a87824f8375e6b2b36b55001dad611230516c724e9fba",
        "arm64" => "sha256:4eee950fb9d134cbf4e228ea3906eb4c7403323334af013c443302f7b74f2737",
        _ => {
            return Err(BuildPlanError::UnsupportedPlatform {
                platform: platform.clone(),
            });
        }
    };
    let buildkit_manifest_digest = digest(buildkit)?;
    Ok((
        format!("moby/buildkit@{buildkit}"),
        buildkit_manifest_digest,
    ))
}

pub(super) fn lower_build_adapter(
    checkout: &CheckedOutGitSource,
    adapter: &BuildAdapter,
    platform: &OciPlatform,
    workspace: &Path,
    toolchain: &BuildToolchain,
) -> Result<BuildExecutionPlan, BuildPlanError> {
    let context = container_path(workspace, checkout.context())?;
    let output = workspace.join("oci");
    let oci_layout = container_path(workspace, &output)?;
    let platform = format!("{}/{}", platform.os(), platform.architecture());
    let commit = checkout.commit().commit.as_str();
    let mut arguments = vec![
        "build".to_owned(),
        "--local".to_owned(),
        format!("context={context}"),
    ];
    let prepare = match (adapter, &toolchain.adapter) {
        (BuildAdapter::Dockerfile { dockerfile, target }, BuildAdapterToolchain::Dockerfile) => {
            let dockerfile_path = checkout.resolve_context_path(dockerfile).map_err(|error| {
                BuildPlanError::UnsafePath {
                    message: error.to_string(),
                }
            })?;
            let dockerfile = dockerfile_path
                .strip_prefix(checkout.context())
                .map_err(|_| BuildPlanError::UnsafePath {
                    message: "Dockerfile escaped the build context".to_owned(),
                })?;
            arguments.extend([
                "--local".to_owned(),
                format!("dockerfile={context}"),
                "--frontend".to_owned(),
                "dockerfile.v0".to_owned(),
                "--opt".to_owned(),
                format!("filename={}", dockerfile.to_string_lossy()),
                "--opt".to_owned(),
                format!("platform={platform}"),
                "--opt".to_owned(),
                format!("build-arg:PLOYZ_GIT_COMMIT={commit}"),
            ]);
            if let Some(target) = target {
                arguments.extend(["--opt".to_owned(), format!("target={}", target.as_str())]);
            }
            None
        }
        (
            BuildAdapter::Railpack { cache_scope },
            BuildAdapterToolchain::Railpack {
                helper_path,
                helper_version: _,
                helper_sha256: _,
                frontend_reference,
                frontend_manifest_digest: _,
            },
        ) => {
            let plan_path = workspace.join("railpack-plan.json");
            let info_path = workspace.join("railpack-info.json");
            let dockerfile = container_path(workspace, workspace)?;
            arguments.extend([
                "--local".to_owned(),
                format!("dockerfile={dockerfile}"),
                "--frontend".to_owned(),
                "gateway.v0".to_owned(),
                "--opt".to_owned(),
                format!("source={frontend_reference}"),
                "--opt".to_owned(),
                format!("platform={platform}"),
                "--opt".to_owned(),
                format!(
                    "build-arg:cache-key={}",
                    RailpackCacheKey::derive(cache_scope).as_str()
                ),
                "--opt".to_owned(),
                format!("build-arg:PLOYZ_GIT_COMMIT={commit}"),
            ]);
            Some(PrepareCommand {
                program: helper_path.clone(),
                arguments: vec![
                    "prepare".to_owned(),
                    checkout.context().to_string_lossy().into_owned(),
                    "--plan-out".to_owned(),
                    plan_path.to_string_lossy().into_owned(),
                    "--info-out".to_owned(),
                    info_path.to_string_lossy().into_owned(),
                ],
            })
        }
        (BuildAdapter::Dockerfile { .. }, BuildAdapterToolchain::Railpack { .. })
        | (BuildAdapter::Railpack { .. }, BuildAdapterToolchain::Dockerfile) => {
            return Err(BuildPlanError::AdapterToolchainMismatch);
        }
    };
    arguments.extend([
        "--output".to_owned(),
        format!("type=oci,dest={oci_layout},tar=false"),
    ]);
    Ok(BuildExecutionPlan {
        prepare,
        buildctl_arguments: arguments,
        oci_layout: output,
    })
}

fn container_path(workspace: &Path, path: &Path) -> Result<String, BuildPlanError> {
    let relative = path
        .strip_prefix(workspace)
        .map_err(|_| BuildPlanError::UnsafePath {
            message: format!("path {path:?} is outside build workspace {workspace:?}"),
        })?;
    Ok(Path::new("/workspace")
        .join(relative)
        .to_string_lossy()
        .into_owned())
}

fn digest(value: &str) -> Result<OciDigest, BuildPlanError> {
    OciDigest::try_new(value).map_err(|error| invalid_pin(error.to_string()))
}

fn invalid_pin(message: String) -> BuildPlanError {
    BuildPlanError::InvalidToolchainPin { message }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildPlanError {
    #[error("build platform is unsupported by the pinned toolchain: {platform:?}")]
    UnsupportedPlatform { platform: OciPlatform },
    #[error("build adapter does not match its selected toolchain")]
    AdapterToolchainMismatch,
    #[error("build path is unsafe: {message}")]
    UnsafePath { message: String },
    #[error("pinned build toolchain is invalid: {message}")]
    InvalidToolchainPin { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{BuildCacheScope, BuildContextPath, GitSource};
    use std::fs;

    fn checkout(temp: &tempfile::TempDir) -> CheckedOutGitSource {
        let root = temp.path().join("source");
        let context = root.join("app");
        fs::create_dir_all(&context).expect("context");
        fs::write(context.join("Dockerfile"), "FROM scratch\n").expect("dockerfile");
        let source = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            Some("app"),
        )
        .expect("source");
        CheckedOutGitSource {
            context: fs::canonicalize(context).expect("context"),
            commit: ployz_core::build::VerifiedGitCommit::from_source(&source),
        }
    }

    fn workspace(temp: &tempfile::TempDir) -> PathBuf {
        fs::canonicalize(temp.path()).expect("workspace")
    }

    #[test]
    fn dockerfile_lowers_to_one_native_build_with_verified_commit() {
        let temp = tempfile::tempdir().expect("temp");
        let checkout = checkout(&temp);
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let adapter = BuildAdapter::Dockerfile {
            dockerfile: BuildContextPath::try_new("Dockerfile").expect("path"),
            target: None,
        };
        let toolchain = toolchain_for_platform(&platform, &adapter).expect("toolchain");
        let plan = lower_build_adapter(
            &checkout,
            &adapter,
            &platform,
            &workspace(&temp),
            &toolchain,
        )
        .expect("plan");
        assert!(plan.prepare.is_none());
        assert!(
            plan.buildctl_arguments
                .contains(&"platform=linux/amd64".to_owned())
        );
        assert!(plan.buildctl_arguments.contains(
            &"build-arg:PLOYZ_GIT_COMMIT=0123456789abcdef0123456789abcdef01234567".to_owned()
        ));
        assert!(!format!("{plan:?}").contains("secret"));
    }

    #[test]
    fn railpack_lowers_to_verified_prepare_and_digest_pinned_frontend() {
        let temp = tempfile::tempdir().expect("temp");
        let checkout = checkout(&temp);
        let platform = OciPlatform::try_new("linux", "arm64").expect("platform");
        let adapter = BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new("scope_01J0Y1").expect("scope"),
        };
        let toolchain = toolchain_for_platform(&platform, &adapter).expect("toolchain");
        let plan = lower_build_adapter(
            &checkout,
            &adapter,
            &platform,
            &workspace(&temp),
            &toolchain,
        )
        .expect("plan");
        let prepare = plan.prepare.expect("prepare");
        assert_eq!(
            prepare.program,
            Path::new(railpack_pins().expect("pins").install_path())
        );
        assert!(prepare.arguments.contains(&"--plan-out".to_owned()));
        assert!(!prepare.arguments.contains(&"--env".to_owned()));
        let pins = railpack_pins().expect("pins");
        assert!(plan.buildctl_arguments.contains(&format!(
            "source={}@{}",
            pins.frontend_reference(),
            pins.for_architecture("arm64")
                .expect("arm64 pins")
                .frontend_digest()
        )));
        assert!(plan.buildctl_arguments.contains(
            &"build-arg:PLOYZ_GIT_COMMIT=0123456789abcdef0123456789abcdef01234567".to_owned()
        ));
        assert!(
            plan.buildctl_arguments
                .iter()
                .any(|argument| { argument.starts_with("build-arg:cache-key=ployz-railpack-v1-") })
        );
    }

    #[test]
    fn toolchain_pins_platform_specific_descriptors() {
        let adapter = BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new("scope").expect("scope"),
        };
        let amd = toolchain_for_platform(
            &OciPlatform::try_new("linux", "amd64").expect("platform"),
            &adapter,
        )
        .expect("amd");
        let arm = toolchain_for_platform(
            &OciPlatform::try_new("linux", "arm64").expect("platform"),
            &adapter,
        )
        .expect("arm");
        assert_ne!(amd.buildkit_manifest_digest, arm.buildkit_manifest_digest);
        assert!(amd.buildkit_reference.contains("@sha256:"));
    }
}
