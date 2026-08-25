//! Atomic Zentinel validation, activation, reload, and confirmation.

use std::{
    fs::{self, File},
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use bollard::{
    models::{ContainerCreateBody, HostConfig, Mount, MountType},
    query_parameters::{KillContainerOptionsBuilder, RemoveContainerOptionsBuilder},
};
use futures_util::StreamExt;
use ployz_core::ContainerId;
use thiserror::Error;

use crate::{
    docker::{Error as DockerError, LocalDocker},
    filesystem::atomic_write,
    ingress::IngressProjection,
};

use super::{
    self as zentinel, ADMIN_ADDRESS, projection_digest, render, set_group, write_support_files,
};

const CONFIRMATION_INTERVAL: Duration = Duration::from_millis(100);
const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Failure while applying or confirming Zentinel configuration.
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// Rendering or support-file preparation failed.
    #[error(transparent)]
    Ingress(#[from] zentinel::Error),
    /// Candidate creation or activation failed.
    #[error("Zentinel apply filesystem operation failed: {0}")]
    Filesystem(#[from] io::Error),
    /// Docker could not run the selected image's candidate validation.
    #[error("cannot run Zentinel candidate validation: {0}")]
    Validation(#[source] DockerError),
    /// The selected Zentinel image rejected the rendered candidate.
    #[error("Zentinel rejected projection {digest}: {reason}")]
    ValidationRejected { digest: String, reason: String },
    /// Docker could not signal the running Zentinel container.
    #[error("cannot signal Zentinel reload: {0}")]
    Reload(#[source] DockerError),
    /// The private administration endpoint could not be queried.
    #[error("cannot query Zentinel active configuration: {0}")]
    Admin(#[source] reqwest::Error),
    /// The private administration endpoint returned an invalid response.
    #[error("invalid Zentinel active-configuration response: {0}")]
    AdminResponse(#[source] serde_json::Error),
}

/// Result of running the exact selected image's one-shot schema validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValidationOutcome {
    /// The candidate passed `zentinel test`.
    Accepted,
    /// The selected image rejected the candidate with this diagnostic.
    Rejected(String),
}

/// Observable result after activating and asking Zentinel to reload a projection.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ApplyOutcome {
    /// The private administration endpoint reported the expected digest.
    Confirmed {
        /// Digest of the activated projection.
        digest: String,
    },
    /// Confirmation timed out after the reload request.
    ReloadUnconfirmed {
        /// Digest of the activated projection.
        digest: String,
        /// Last valid digest observed before the timeout, if any.
        last_observed_digest: Option<String>,
    },
}

/// External operations at the Zentinel apply seam.
pub(crate) trait ApplyIo: Sync {
    /// Validate one candidate using the exact selected image.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] when Docker cannot run validation.
    async fn validate_candidate(
        &self,
        image: &str,
        candidate: &Path,
    ) -> Result<ValidationOutcome, Error>;

    /// Send `SIGHUP` to the selected running Ingress Proxy container.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Reload`] when Docker cannot deliver the signal.
    async fn signal_reload(&self, container: &ContainerId) -> Result<(), Error>;

    /// Read the currently active projection digest, when present.
    ///
    /// # Errors
    ///
    /// Returns a typed administration transport or response error.
    async fn active_digest(&self) -> Result<Option<String>, Error>;
}

impl ApplyIo for (&LocalDocker, &reqwest::Client) {
    async fn validate_candidate(
        &self,
        image: &str,
        candidate: &Path,
    ) -> Result<ValidationOutcome, Error> {
        validate_candidate(self.0, image, candidate)
            .await
            .map_err(Error::Validation)
    }

    async fn signal_reload(&self, container: &ContainerId) -> Result<(), Error> {
        self.0
            .client()
            .kill_container(
                container.as_str(),
                Some(
                    KillContainerOptionsBuilder::default()
                        .signal("SIGHUP")
                        .build(),
                ),
            )
            .await
            .map_err(DockerError::Docker)
            .map_err(Error::Reload)
    }

    async fn active_digest(&self) -> Result<Option<String>, Error> {
        let body = self
            .1
            .get(format!("http://{ADMIN_ADDRESS}/config"))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(Error::Admin)?
            .text()
            .await
            .map_err(Error::Admin)?;
        active_digest(&body).map_err(Error::AdminResponse)
    }
}

/// Validate, atomically activate, reload, and confirm one Zentinel projection.
///
/// # Errors
///
/// Returns a typed phase error when rendering, filesystem work, Docker, or the
/// selected image rejects the candidate. Confirmation timeout is an outcome.
pub(crate) async fn apply<I: ApplyIo>(
    projection: &IngressProjection,
    live_config: &Path,
    selected_image: &str,
    ingress_container: &ContainerId,
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

    let candidate = match write_candidate(live_config, rendered.kdl()) {
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

    if let Err(error) = write_support_files(projection, live_config) {
        record_evidence(&digest, Evidence::ActivationFailed);
        return Err(Error::Ingress(error));
    }
    match candidate.activate(live_config) {
        Ok(Activation::Durable) => record_evidence(&digest, Evidence::Activated),
        Ok(Activation::Unsynced(error)) => {
            record_evidence(&digest, Evidence::ActivatedUnsynced);
            tracing::warn!(%error, "failed to sync activated Zentinel configuration directory");
        }
        Err(error) => {
            record_evidence(&digest, Evidence::ActivationFailed);
            return Err(Error::Filesystem(error));
        }
    }

    if let Err(error) = io.signal_reload(ingress_container).await {
        record_evidence(&digest, Evidence::ReloadFailed);
        return Err(error);
    }
    record_evidence(&digest, Evidence::ReloadRequested);

    Ok(confirm(io, digest).await)
}

async fn confirm<I: ApplyIo>(io: &I, digest: String) -> ApplyOutcome {
    let deadline = tokio::time::Instant::now() + CONFIRMATION_TIMEOUT;
    let mut last_observed_digest = None;
    loop {
        match tokio::time::timeout_at(deadline, io.active_digest()).await {
            Ok(Ok(Some(observed))) if observed == digest => {
                record_evidence(&digest, Evidence::Confirmed(&observed));
                return ApplyOutcome::Confirmed { digest };
            }
            Ok(Ok(observed)) => last_observed_digest = observed,
            Ok(Err(error)) => {
                record_evidence(&digest, Evidence::PollFailed);
                tracing::debug!(%error, "Zentinel active-configuration poll failed");
            }
            Err(_) => break,
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        tokio::time::sleep_until(std::cmp::min(deadline, now + CONFIRMATION_INTERVAL)).await;
    }
    record_evidence(
        &digest,
        Evidence::ReloadUnconfirmed(last_observed_digest.as_deref()),
    );
    ApplyOutcome::ReloadUnconfirmed {
        digest,
        last_observed_digest,
    }
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
        match docker
            .client()
            .wait_container(&created.id, None)
            .next()
            .await
        {
            Some(Ok(_)) => Ok(ValidationOutcome::Accepted),
            Some(Err(bollard::errors::Error::DockerContainerWaitError { error, code })) => Ok(
                ValidationOutcome::Rejected(format!("exit status {code}: {error}")),
            ),
            Some(Err(error)) => Err(DockerError::Docker(error)),
            None => Err(DockerError::InvalidContainerConfig(
                "Zentinel validation wait ended without an exit status".into(),
            )),
        }
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
                tracing::warn!(%error, "failed to remove rejected Zentinel validator");
            }
            Ok(ValidationOutcome::Rejected(reason))
        }
        validation => {
            cleanup?;
            validation
        }
    }
}

fn validation_container_body(
    image: &str,
    candidate: &Path,
) -> Result<ContainerCreateBody, DockerError> {
    let candidate = candidate.canonicalize()?;
    let candidate_source = candidate.to_str().ok_or_else(|| {
        DockerError::InvalidContainerConfig("Zentinel candidate path is not UTF-8".into())
    })?;
    let support_source = candidate.parent().and_then(Path::to_str).ok_or_else(|| {
        DockerError::InvalidContainerConfig("Zentinel candidate directory is not UTF-8".into())
    })?;
    let read_only_bind = |source: &str, target: &str| Mount {
        target: Some(target.into()),
        source: Some(source.into()),
        typ: Some(MountType::BIND),
        read_only: Some(true),
        ..Default::default()
    };
    Ok(ContainerCreateBody {
        image: Some(image.to_owned()),
        // The selected image's entrypoint is already `/zentinel`.
        cmd: Some(vec!["test".into(), "-c".into(), "/candidate.kdl".into()]),
        host_config: Some(HostConfig {
            mounts: Some(vec![
                read_only_bind(candidate_source, "/candidate.kdl"),
                read_only_bind(support_source, "/config"),
            ]),
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[derive(Clone, Copy)]
enum Evidence<'digest> {
    Rendered,
    RenderFailed,
    ValidationAccepted,
    ValidationRejected,
    ValidationFailed,
    CandidateWriteFailed,
    Activated,
    ActivatedUnsynced,
    ActivationFailed,
    ReloadRequested,
    ReloadFailed,
    PollFailed,
    Confirmed(&'digest str),
    ReloadUnconfirmed(Option<&'digest str>),
}

impl<'digest> Evidence<'digest> {
    const fn fields(self) -> (&'static str, &'static str, Option<&'digest str>) {
        match self {
            Self::Rendered => ("render", "rendered", None),
            Self::RenderFailed => ("render", "failed", None),
            Self::ValidationAccepted => ("validation", "accepted", None),
            Self::ValidationRejected => ("validation", "rejected", None),
            Self::ValidationFailed => ("validation", "failed", None),
            Self::CandidateWriteFailed => ("validation", "candidate_write_failed", None),
            Self::Activated => ("activation", "activated", None),
            Self::ActivatedUnsynced => ("activation", "activated_unsynced", None),
            Self::ActivationFailed => ("activation", "failed", None),
            Self::ReloadRequested => ("reload", "requested", None),
            Self::ReloadFailed => ("reload", "failed", None),
            Self::PollFailed => ("confirmation", "poll_failed", None),
            Self::Confirmed(observed) => ("confirmation", "confirmed", Some(observed)),
            Self::ReloadUnconfirmed(last) => ("confirmation", "reload_unconfirmed", last),
        }
    }
}

fn record_evidence(digest: &str, evidence: Evidence<'_>) {
    let (phase, outcome, last_observed_digest) = evidence.fields();
    tracing::info!(
        backend = "zentinel",
        projection_digest = digest,
        phase,
        outcome,
        last_observed_digest = last_observed_digest.unwrap_or_default(),
        "Ingress Proxy apply evidence"
    );
}

fn write_candidate(live_config: &Path, contents: &str) -> io::Result<Candidate> {
    let parent = live_config
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o750))?;
    set_group(parent)?;
    // ponytail: the single ingress watcher serializes apply; use unique candidates if that changes.
    let path = live_config.with_extension("candidate.kdl");
    atomic_write(&path, contents.as_bytes(), 0o640)?;
    let candidate = Candidate(Some(path));
    set_group(candidate.path())?;
    Ok(candidate)
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
        fs::rename(self.path(), live_config)?;
        self.0 = None;
        Ok(
            match sync_parent(
                live_config
                    .parent()
                    .expect("candidate and live path share a parent"),
            ) {
                Ok(()) => Activation::Durable,
                Err(error) => Activation::Unsynced(error),
            },
        )
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        let Some(candidate) = self.0.as_deref() else {
            return;
        };
        if let Err(error) = fs::remove_file(candidate)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %candidate.display(), %error, "failed to remove unactivated Zentinel candidate");
        }
    }
}

#[derive(serde::Deserialize)]
struct ActiveConfigResponse {
    config: ActiveConfig,
}

#[derive(serde::Deserialize)]
struct ActiveConfig {
    listeners: Vec<ActiveListener>,
}

#[derive(serde::Deserialize)]
struct ActiveListener {
    id: String,
}

/// Extract the projection digest encoded in the private administration listener.
///
/// # Errors
///
/// Returns when the response does not match the private administration schema.
pub(crate) fn active_digest(response: &str) -> Result<Option<String>, serde_json::Error> {
    let response: ActiveConfigResponse = serde_json::from_str(response)?;
    Ok(response.config.listeners.into_iter().find_map(|listener| {
        let digest = listener.id.strip_prefix("ployz-admin-")?;
        (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| digest.to_owned())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validator_mounts_the_candidate_and_its_support_root_read_only() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-zentinel-validation-mounts-{}",
            ployz_core::MachineId::random()
        ));
        let live = root.join("zentinel.kdl");
        let candidate = write_candidate(&live, "candidate configuration").unwrap();

        let body = validation_container_body("zentinel:test", candidate.path()).unwrap();
        assert_eq!(
            body.cmd.unwrap(),
            ["test", "-c", "/candidate.kdl"].map(str::to_owned)
        );
        let mounts = body.host_config.unwrap().mounts.unwrap();
        let [candidate_mount, support_mount] = mounts.as_slice() else {
            panic!("expected candidate and support-root mounts: {mounts:?}");
        };
        assert!(mounts.iter().all(|mount| mount.read_only == Some(true)));
        assert_eq!(candidate_mount.target.as_deref(), Some("/candidate.kdl"));
        assert_eq!(support_mount.target.as_deref(), Some("/config"));
        assert_eq!(support_mount.source.as_deref(), root.to_str());

        drop(candidate);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn post_rename_sync_failure_is_a_committed_activation() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-zentinel-activation-unsynced-{}",
            ployz_core::MachineId::random()
        ));
        let live = root.join("zentinel.kdl");
        let candidate = write_candidate(&live, "candidate configuration").unwrap();

        let outcome = candidate
            .activate_with(&live, |_| Err(io::Error::other("sync failed")))
            .unwrap();

        assert!(matches!(outcome, Activation::Unsynced(_)));
        assert_eq!(
            fs::read_to_string(&live).unwrap(),
            "candidate configuration"
        );
        assert!(!live.with_extension("candidate.kdl").exists());

        fs::remove_dir_all(root).unwrap();
    }
}
