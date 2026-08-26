//! Atomic Envoy validation and file-watched xDS activation.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use bollard::{
    models::{ContainerCreateBody, ContainerWaitResponse, HostConfig, Mount, MountType},
    query_parameters::RemoveContainerOptionsBuilder,
};
use futures_util::StreamExt;
use thiserror::Error;

use crate::{
    docker::{Error as DockerError, LocalDocker},
    filesystem::atomic_write,
    ingress::{IngressProjection, remove_stale_certificate_files, write_certificate_files},
};

use super::{
    self as envoy, BOOTSTRAP, CDS_FILE, LDS_FILE, RDS_FILE, SDS_FILE, projection_digest, render,
    write_xds,
};

/// Failure while applying Envoy configuration.
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// Rendering failed.
    #[error(transparent)]
    Ingress(#[from] envoy::Error),
    /// Candidate creation or activation failed.
    #[error("Envoy apply filesystem operation failed: {0}")]
    Filesystem(#[from] io::Error),
    /// Docker could not run the selected image's candidate validation.
    #[error("cannot run Envoy candidate validation: {0}")]
    Validation(#[source] DockerError),
    /// The selected Envoy image rejected the rendered candidate.
    #[error("Envoy rejected projection {digest}: {reason}")]
    ValidationRejected { digest: String, reason: String },
}

/// Result of running the exact selected image's one-shot schema validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValidationOutcome {
    /// The candidate passed `envoy --mode validate`.
    Accepted,
    /// The selected image rejected the candidate with this diagnostic.
    Rejected(String),
}

/// Observable result after activating a validated Envoy projection.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ApplyOutcome {
    /// Live xDS files were replaced; Envoy's file watch consumes them.
    Activated {
        /// Digest of the activated projection.
        digest: String,
    },
}

/// External operations at the Envoy apply seam.
pub(crate) trait ApplyIo: Sync {
    /// Validate one candidate directory using the exact selected image.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] when Docker cannot run validation.
    async fn validate_candidate(
        &self,
        image: &str,
        candidate: &Path,
    ) -> Result<ValidationOutcome, Error>;
}

impl ApplyIo for LocalDocker {
    async fn validate_candidate(
        &self,
        image: &str,
        candidate: &Path,
    ) -> Result<ValidationOutcome, Error> {
        validate_candidate(self, image, candidate)
            .await
            .map_err(Error::Validation)
    }
}

/// Validate, then atomically activate one Envoy projection.
///
/// A rejected candidate is removed and the previously active files keep serving.
/// There is no admin mutation and no container signal.
///
/// # Errors
///
/// Returns a typed phase error when rendering, filesystem work, Docker, or the
/// selected image rejects the candidate.
pub(crate) async fn apply<I: ApplyIo>(
    projection: &IngressProjection,
    live_config: &Path,
    selected_image: &str,
    io: &I,
) -> Result<ApplyOutcome, Error> {
    let rendered = match render(projection) {
        Ok(rendered) => rendered,
        Err(error) => {
            record_evidence(&projection_digest(projection), Evidence::RenderFailed);
            return Err(Error::Ingress(error));
        }
    };
    let digest = rendered.digest().to_owned();
    record_evidence(&digest, Evidence::Rendered);

    let candidate = match write_candidate(live_config, projection, &rendered) {
        Ok(candidate) => candidate,
        Err(error) => {
            record_evidence(&digest, Evidence::CandidateWriteFailed);
            return Err(Error::Filesystem(error));
        }
    };
    let validation = match io
        .validate_candidate(selected_image, candidate.path())
        .await
    {
        Ok(validation) => validation,
        Err(error) => {
            record_evidence(&digest, Evidence::ValidationFailed);
            return Err(error);
        }
    };
    if let ValidationOutcome::Rejected(reason) = validation {
        record_evidence(&digest, Evidence::ValidationRejected);
        return Err(Error::ValidationRejected { digest, reason });
    }
    record_evidence(&digest, Evidence::ValidationAccepted);

    if let Err(error) = write_envoy_certificates(live_config, projection) {
        record_evidence(&digest, Evidence::ActivationFailed);
        return Err(Error::Filesystem(error));
    }
    match candidate.activate(live_config) {
        Ok(Activation::Durable) => record_evidence(&digest, Evidence::Activated),
        Ok(Activation::Unsynced(error)) => {
            record_evidence(&digest, Evidence::ActivatedUnsynced);
            tracing::warn!(%error, "failed to sync activated Envoy configuration directory");
        }
        Err(error) => {
            record_evidence(&digest, Evidence::ActivationFailed);
            return Err(Error::Filesystem(error));
        }
    }
    if let Err(error) = remove_stale_certificate_files(live_config, &projection.sites) {
        record_evidence(&digest, Evidence::ActivationFailed);
        return Err(Error::Filesystem(error));
    }

    Ok(ApplyOutcome::Activated { digest })
}

async fn validate_candidate(
    docker: &LocalDocker,
    image: &str,
    candidate: &Path,
) -> Result<ValidationOutcome, DockerError> {
    let body = validation_container_body(image, candidate)?;
    let created = docker.client().create_container(None, body).await?;
    let validation = async {
        docker.client().start_container(&created.id, None).await?;
        classify_validation_wait(
            docker
                .client()
                .wait_container(&created.id, None)
                .next()
                .await,
        )
    }
    .await;
    let cleanup = docker
        .client()
        .remove_container(
            &created.id,
            Some(RemoveContainerOptionsBuilder::default().force(true).build()),
        )
        .await
        .map_err(DockerError::Docker);
    match validation {
        Ok(ValidationOutcome::Rejected(reason)) => {
            if let Err(error) = cleanup {
                tracing::warn!(%error, "failed to remove rejected Envoy validator");
            }
            Ok(ValidationOutcome::Rejected(reason))
        }
        validation => {
            cleanup?;
            validation
        }
    }
}

// Wait stream item from `wait_container`. Bollard converts some nonzero exits
// to DockerContainerWaitError; a successful body can still carry status_code.
fn classify_validation_wait(
    wait: Option<Result<ContainerWaitResponse, bollard::errors::Error>>,
) -> Result<ValidationOutcome, DockerError> {
    match wait {
        Some(Ok(response)) if response.status_code == 0 => Ok(ValidationOutcome::Accepted),
        Some(Ok(response)) => Ok(ValidationOutcome::Rejected(format!(
            "exit status {}",
            response.status_code
        ))),
        Some(Err(bollard::errors::Error::DockerContainerWaitError { error, code })) => Ok(
            ValidationOutcome::Rejected(format!("exit status {code}: {error}")),
        ),
        Some(Err(error)) => Err(DockerError::Docker(error)),
        None => Err(DockerError::InvalidContainerConfig(
            "Envoy validation wait ended without an exit status".into(),
        )),
    }
}

fn validation_container_body(
    image: &str,
    candidate: &Path,
) -> Result<ContainerCreateBody, DockerError> {
    let candidate = candidate.canonicalize()?;
    let candidate_source = candidate.to_str().ok_or_else(|| {
        DockerError::InvalidContainerConfig("Envoy candidate path is not UTF-8".into())
    })?;
    Ok(ContainerCreateBody {
        image: Some(image.to_owned()),
        cmd: Some(vec![
            "envoy".into(),
            "--mode".into(),
            "validate".into(),
            "-c".into(),
            "/config/bootstrap.yaml".into(),
        ]),
        host_config: Some(HostConfig {
            mounts: Some(vec![Mount {
                target: Some("/config".into()),
                source: Some(candidate_source.into()),
                typ: Some(MountType::BIND),
                read_only: Some(true),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[derive(Clone, Copy)]
enum Evidence {
    Rendered,
    RenderFailed,
    ValidationAccepted,
    ValidationRejected,
    ValidationFailed,
    CandidateWriteFailed,
    Activated,
    ActivatedUnsynced,
    ActivationFailed,
}

impl Evidence {
    const fn fields(self) -> (&'static str, &'static str) {
        match self {
            Self::Rendered => ("render", "rendered"),
            Self::RenderFailed => ("render", "failed"),
            Self::ValidationAccepted => ("validation", "accepted"),
            Self::ValidationRejected => ("validation", "rejected"),
            Self::ValidationFailed => ("validation", "failed"),
            Self::CandidateWriteFailed => ("validation", "candidate_write_failed"),
            Self::Activated => ("activation", "activated"),
            Self::ActivatedUnsynced => ("activation", "activated_unsynced"),
            Self::ActivationFailed => ("activation", "failed"),
        }
    }
}

fn record_evidence(digest: &str, evidence: Evidence) {
    let (phase, outcome) = evidence.fields();
    tracing::info!(
        backend = "envoy",
        projection_digest = digest,
        phase,
        outcome,
        "Ingress Proxy apply evidence"
    );
}

fn write_candidate(
    live_config: &Path,
    projection: &IngressProjection,
    rendered: &envoy::RenderedConfig,
) -> io::Result<Candidate> {
    let parent = live_config
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent"))?;
    fs::create_dir_all(parent)?;
    let path = parent.join(".apply-candidate");
    if path.exists() {
        fs::remove_dir_all(&path)?;
    }
    fs::create_dir_all(&path)?;
    atomic_write(&path.join("bootstrap.yaml"), BOOTSTRAP.as_bytes(), 0o644)?;
    write_xds(&path, rendered)?;
    write_envoy_certificates(&path.join("bootstrap.yaml"), projection)?;
    Ok(Candidate(Some(path)))
}

/// Envoy runs as uid 101; root-owned 0600 keys would be unreadable in the container.
const ENVOY_PRIVATE_KEY_MODE: u32 = 0o644;

fn write_envoy_certificates(config_file: &Path, projection: &IngressProjection) -> io::Result<()> {
    write_certificate_files(
        config_file,
        &projection.sites,
        ENVOY_PRIVATE_KEY_MODE,
        create_directory,
        skip_group,
    )
}

fn create_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

fn skip_group(_: &Path) -> io::Result<()> {
    Ok(())
}

struct Candidate(Option<PathBuf>);

enum Activation {
    Durable,
    Unsynced(io::Error),
}

impl Candidate {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("candidate is not yet activated")
    }

    fn activate(self, live_config: &Path) -> io::Result<Activation> {
        self.activate_with(live_config, |parent| File::open(parent)?.sync_all())
    }

    fn activate_with(
        mut self,
        live_config: &Path,
        sync_parent: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Activation> {
        let live_parent = live_config.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent")
        })?;
        let candidate = self.0.take().expect("candidate is not yet activated");
        // Clusters, secrets, routes, then listeners so Envoy never binds TLS or
        // routes against missing SDS or CDS resources.
        for name in [CDS_FILE, SDS_FILE, RDS_FILE, LDS_FILE] {
            fs::rename(candidate.join(name), live_parent.join(name))?;
        }
        let leftover = fs::remove_dir_all(&candidate);
        Ok(match (sync_parent(live_parent), leftover) {
            (Ok(()), Ok(())) => Activation::Durable,
            (Err(error), _) | (Ok(()), Err(error)) => Activation::Unsynced(error),
        })
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        let Some(candidate) = self.0.as_deref() else {
            return;
        };
        if let Err(error) = fs::remove_dir_all(candidate)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %candidate.display(), %error, "failed to remove unactivated Envoy candidate");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validator_mounts_the_candidate_directory_read_only() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-envoy-validation-mounts-{}",
            ployz_core::MachineId::random()
        ));
        let live = root.join("bootstrap.yaml");
        let projection = crate::ingress::tests::renderer_projection();
        let rendered = render(&projection).unwrap();
        let candidate = write_candidate(&live, &projection, &rendered).unwrap();

        let body = validation_container_body("envoy:test", candidate.path()).unwrap();
        assert_eq!(
            body.cmd.unwrap(),
            [
                "envoy",
                "--mode",
                "validate",
                "-c",
                "/config/bootstrap.yaml"
            ]
            .map(str::to_owned)
        );
        let mounts = body.host_config.unwrap().mounts.unwrap();
        let [candidate_mount] = mounts.as_slice() else {
            panic!("expected one candidate-root mount: {mounts:?}");
        };
        assert_eq!(candidate_mount.read_only, Some(true));
        assert_eq!(candidate_mount.target.as_deref(), Some("/config"));
        assert_eq!(candidate_mount.source.as_deref(), candidate.path().to_str());

        drop(candidate);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nonzero_wait_status_rejects_the_candidate() {
        let wait = Some(Ok(ContainerWaitResponse {
            status_code: 1,
            error: Some(bollard::models::ContainerWaitExitError { message: None }),
        }));
        assert_eq!(
            classify_validation_wait(wait).unwrap(),
            ValidationOutcome::Rejected("exit status 1".into())
        );
    }

    #[test]
    fn zero_wait_status_accepts_the_candidate() {
        let wait = Some(Ok(ContainerWaitResponse {
            status_code: 0,
            error: None,
        }));
        assert_eq!(
            classify_validation_wait(wait).unwrap(),
            ValidationOutcome::Accepted
        );
    }

    #[test]
    fn post_rename_sync_failure_is_a_committed_activation() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-envoy-activation-unsynced-{}",
            ployz_core::MachineId::random()
        ));
        let live = root.join("bootstrap.yaml");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(LDS_FILE), "live-lds\n").unwrap();
        let projection = crate::ingress::tests::renderer_projection();
        let rendered = render(&projection).unwrap();
        let candidate = write_candidate(&live, &projection, &rendered).unwrap();

        let outcome = candidate
            .activate_with(&live, |_| Err(io::Error::other("sync failed")))
            .unwrap();

        assert!(matches!(outcome, Activation::Unsynced(_)));
        assert_eq!(
            fs::read_to_string(root.join(LDS_FILE)).unwrap(),
            rendered.lds()
        );
        assert!(!root.join(".apply-candidate").exists());

        fs::remove_dir_all(root).unwrap();
    }
}
