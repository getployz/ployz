//! Images on this Machine: a deploy's registry pull, and Image Cleanup, which finds
//! each repository's superseded build images and removes them unforced, keeping any a
//! Container uses.
//!
//! Only `repository:ployz-sha256-*` tags, which Direct Image Transfer creates, are
//! candidates.

use std::collections::HashSet;

use bollard::{
    errors::Error as DockerError,
    query_parameters::{ListContainersOptionsBuilder, RemoveImageOptions},
};
use ployz_core::{
    DiskFull, DiskSpace, ExposedPort, ImageRemoval, ImageRemovalOutcome, ImageSummary,
    ImagesRemoved, PullPolicy, RegistryAuth,
};

use super::{ContainerRuntime, Error};

const RETENTION_TAG: &str = ":ployz-sha256-";
/// Unused images kept for rollback.
const KEEP: usize = 3;
/// Unused images kept when the Docker root is nearly full.
const KEEP_UNDER_PRESSURE: usize = 1;
/// Beyond [`KEEP`], an unused image goes once this idle.
const IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;
/// Below this free share of the Docker root, keep only [`KEEP_UNDER_PRESSURE`].
const PRESSURE_FREE_PERCENT: u64 = 20;
/// Below this free under the Docker root, a failed layer write is a full disk.
const DISK_FULL_FREE_BYTES: u64 = 1_000_000_000;
/// ENOSPC, as Docker words it.
const OUT_OF_SPACE: &str = "no space left on device";
/// The steps in which containerd writes a pulled layer to disk.
const LAYER_WRITES: [&str; 3] = [
    "failed to extract layer",
    "failed to register layer",
    "failed commit on ref",
];
/// How long a failed pull waits on `docker info` before reporting the failure as is.
const DISK_READ_BOUND: std::time::Duration = std::time::Duration::from_secs(3);

impl ContainerRuntime {
    /// Make `image` present as `policy` asks, confirm Docker holds it, and answer
    /// the ports its config exposes.
    ///
    /// # Errors
    ///
    /// Returns when the pull fails or the image is absent afterwards, as under `Never`.
    pub async fn pull_image(
        &self,
        image: &str,
        policy: PullPolicy,
        auth: Option<&RegistryAuth>,
    ) -> Result<Vec<ExposedPort>, Error> {
        let docker = &self.docker.client;
        if let Err(error) = crate::docker_image::prepare_image(docker, image, policy, auth).await {
            return Err(self.pull_failure(error).await);
        }
        let inspect = docker
            .inspect_image(image)
            .await
            .map_err(Error::ImagePull)?;
        Ok(exposed_ports(
            inspect
                .config
                .and_then(|config| config.exposed_ports)
                .unwrap_or_default(),
        ))
    }

    /// `error` from a pull, as a full disk when the Docker root has almost no free
    /// space left: containerd then fails mid-extract with whatever call ran out.
    /// A `docker info` that fails or stalls past [`DISK_READ_BOUND`] leaves `error`
    /// as it is.
    pub(super) async fn pull_failure(&self, error: DockerError) -> Error {
        let free = tokio::time::timeout(DISK_READ_BOUND, self.docker.client.info())
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|info| info.docker_root_dir)
            .and_then(|root| {
                let (_, free_bytes) = crate::host_capacity::filesystem_space(&root).ok()?;
                Some((root, free_bytes))
            });
        pull_failure(error, free)
    }

    /// Unix seconds this Machine last tagged the image, when Docker recorded it.
    pub(super) async fn last_tagged(&self, id: &str) -> Result<Option<i64>, Error> {
        match self.docker.client.inspect_image(id).await {
            Ok(image) => Ok(image
                .metadata
                .and_then(|metadata| metadata.last_tag_time)
                .and_then(|time| chrono::DateTime::parse_from_rfc3339(&time).ok())
                .map(|time| time.timestamp())
                .filter(|seconds| *seconds > 0)),
            // Removed between list and inspect.
            Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Remove each repository's superseded build images.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot list images or Containers; per-reference failures
    /// are results.
    pub async fn prune_images(&self, repositories: &[String]) -> Result<ImagesRemoved, Error> {
        let now = chrono::Utc::now().timestamp();
        let mut references = Vec::new();
        for repository in repositories {
            let store = self
                .list_images(Some(&format!("{repository}{RETENTION_TAG}*")), true)
                .await?;
            references.extend(superseded(
                &store.images,
                repository,
                under_pressure(store.docker_root),
                now,
            ));
        }
        if references.is_empty() {
            return Ok(ImagesRemoved::default());
        }
        self.remove_images(&references).await
    }

    /// Remove each reference without force. A reference whose image any Container
    /// uses, running or not, is kept: Docker alone would untag it when the image
    /// carries another tag.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot list Containers; per-reference failures are results.
    pub async fn remove_images(&self, references: &[String]) -> Result<ImagesRemoved, Error> {
        let options = ListContainersOptionsBuilder::default().all(true).build();
        let in_use = self
            .docker
            .client
            .list_containers(Some(options))
            .await?
            .into_iter()
            .filter_map(|container| container.image_id)
            .collect::<HashSet<_>>();
        let mut results = Vec::with_capacity(references.len());
        for reference in references {
            let outcome = match self.docker.client.inspect_image(reference).await {
                Ok(image) if image.id.as_ref().is_some_and(|id| in_use.contains(id)) => {
                    ImageRemovalOutcome::InUse
                }
                Ok(_) => match self
                    .docker
                    .client
                    .remove_image(reference, None::<RemoveImageOptions>, None)
                    .await
                {
                    Ok(_) => ImageRemovalOutcome::Removed,
                    Err(error) => refusal(error),
                },
                Err(error) => refusal(error),
            };
            results.push(ImageRemoval {
                reference: reference.clone(),
                outcome,
            });
        }
        Ok(ImagesRemoved { results })
    }
}

/// Retention tags in `repository` to remove, newest kept. Images a Container uses,
/// or whose use Docker did not count, are neither kept nor removed.
fn superseded(images: &[ImageSummary], repository: &str, pressure: bool, now: i64) -> Vec<String> {
    let prefix = format!("{repository}{RETENTION_TAG}");
    let mut unused = images
        .iter()
        .filter(|image| image.containers == 0)
        .flat_map(|image| {
            image
                .repo_tags
                .iter()
                .filter(|tag| tag.starts_with(&prefix))
                .map(|tag| (image.last_tagged, tag))
        })
        .collect::<Vec<_>>();
    // Newest first; an unknown tag time sorts oldest.
    unused.sort_by_key(|(tagged, _)| std::cmp::Reverse(*tagged));
    let keep = if pressure { KEEP_UNDER_PRESSURE } else { KEEP };
    unused
        .into_iter()
        .skip(keep)
        .filter(|(tagged, _)| pressure || tagged.is_some_and(|at| now - at > IDLE_SECONDS))
        .map(|(_, tag)| tag.clone())
        .collect()
}

/// A pull is the full disk, with Docker's error as its cause, when Docker says the
/// device is out of space, or when containerd failed writing a layer with `free`
/// under [`DISK_FULL_FREE_BYTES`]: it then fails with whatever call ran out. Any
/// other failure, a registry's answer among them, stays itself.
fn pull_failure(error: DockerError, free: Option<(String, u64)>) -> Error {
    let message = error.to_string();
    let out_of_space = message.contains(OUT_OF_SPACE);
    let layer_write = LAYER_WRITES.iter().any(|write| message.contains(write));
    match free {
        Some((docker_root, free_bytes))
            if out_of_space || (layer_write && free_bytes < DISK_FULL_FREE_BYTES) =>
        {
            Error::DiskFull {
                full: DiskFull {
                    docker_root,
                    free_bytes,
                },
                source: error,
            }
        }
        _ => Error::ImagePull(error),
    }
}

/// The routable ports among Docker's exposed-port keys, in order and without repeats.
fn exposed_ports(keys: Vec<String>) -> Vec<ExposedPort> {
    let mut ports = keys
        .iter()
        .filter_map(|key| ExposedPort::from_docker(key))
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn under_pressure(space: Option<DiskSpace>) -> bool {
    space.is_some_and(|space| {
        u128::from(space.free_bytes) * 100
            < u128::from(space.total_bytes) * u128::from(PRESSURE_FREE_PERCENT)
    })
}

fn refusal(error: DockerError) -> ImageRemovalOutcome {
    if let DockerError::DockerResponseServerError { status_code, .. } = &error {
        match status_code {
            404 => return ImageRemovalOutcome::NotFound,
            409 => return ImageRemovalOutcome::InUse,
            _ => {}
        }
    }
    ImageRemovalOutcome::Failed {
        message: ployz_core::error_chain::inline(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 100 * 24 * 60 * 60;
    const DAY: i64 = 24 * 60 * 60;

    fn image(tag: &str, days_ago: Option<i64>, containers: i64) -> ImageSummary {
        ImageSummary {
            id: format!("sha256:{tag}"),
            repo_tags: vec![format!("ployz-build/web:ployz-sha256-{tag}")],
            created: 0,
            size: 1,
            containers,
            platforms: Vec::new(),
            last_tagged: days_ago.map(|days| NOW - days * DAY),
        }
    }

    fn removed(images: &[ImageSummary], pressure: bool) -> Vec<String> {
        superseded(images, "ployz-build/web", pressure, NOW)
            .into_iter()
            .map(|tag| {
                tag.trim_start_matches("ployz-build/web:ployz-sha256-")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn exposed_ports_keep_tcp_and_udp_and_drop_the_rest() {
        let ports = exposed_ports(vec![
            "8080/tcp".into(),
            "80/tcp".into(),
            "53/udp".into(),
            "80/tcp".into(),
            "132/sctp".into(),
            "junk".into(),
        ]);
        let ports = ports
            .iter()
            .map(|port| (port.port.get(), port.protocol))
            .collect::<Vec<_>>();
        assert_eq!(
            ports,
            [
                (53, ployz_core::TransportProtocol::Udp),
                (80, ployz_core::TransportProtocol::Tcp),
                (8080, ployz_core::TransportProtocol::Tcp),
            ]
        );
    }

    #[test]
    fn keeps_three_unused_and_removes_older_idle_ones() {
        let images = [
            image("running", Some(30), 1),
            image("uncounted", Some(30), -1),
            image("a", Some(1), 0),
            image("b", Some(2), 0),
            image("c", Some(3), 0),
            image("recent", Some(4), 0),
            image("old", Some(8), 0),
            image("unknown", None, 0),
        ];
        assert_eq!(removed(&images, false), ["old"]);
    }

    #[test]
    fn pressure_keeps_only_the_newest_unused() {
        let images = [
            image("running", Some(30), 1),
            image("a", Some(1), 0),
            image("b", Some(2), 0),
            image("unknown", None, 0),
        ];
        assert_eq!(removed(&images, true), ["b", "unknown"]);
    }

    #[test]
    fn ignores_other_repositories_and_tags() {
        let mut other = image("x", Some(90), 0);
        other.repo_tags = vec![
            "ployz-build/web-api:ployz-sha256-x".into(),
            "ployz-build/web:latest".into(),
        ];
        assert!(removed(&[other.clone(), other.clone(), other.clone(), other], true).is_empty());
    }

    #[test]
    fn pressure_starts_below_a_fifth_free() {
        let space = |free_bytes| {
            Some(DiskSpace {
                total_bytes: 100,
                free_bytes,
            })
        };
        assert!(under_pressure(space(19)));
        assert!(!under_pressure(space(20)));
        assert!(!under_pressure(None));
    }

    /// The deploy output for a pull that containerd failed mid-extract, as the
    /// Server reports `free_bytes` under its Docker root.
    fn failed_pull(free_bytes: u64) -> ployz_store::Failure {
        let lchown = DockerError::DockerStreamError {
            error: "failed to extract layer: failed to Lchown \"/var/lib/containerd/tmpmounts/m/shadow.mo\": no such file or directory".into(),
        };
        let error = pull_failure(lchown, Some(("/var/lib/docker".into(), free_bytes)));
        ployz_store::Failure::of(
            &ployz_core::ExecutionError::Machine {
                action: ployz_core::MachineAction::PullImage,
                error: ployz_core::RpcError::from(&error),
            },
            &[],
        )
    }

    #[test]
    fn a_pull_on_a_full_disk_names_the_disk_and_keeps_dockers_error() {
        let failure = failed_pull(48_000_000);
        assert_eq!(
            failure.reason,
            "The Server's disk is full: 48 MB free under /var/lib/docker"
        );
        assert_eq!(
            failure.cause,
            [
                "Docker stream error: failed to extract layer: failed to Lchown \"/var/lib/containerd/tmpmounts/m/shadow.mo\": no such file or directory"
            ]
        );
    }

    #[test]
    fn a_pull_with_free_space_keeps_dockers_error() {
        let failure = failed_pull(50_000_000_000);
        assert_eq!(failure.reason, "pull image failed");
        assert_eq!(
            failure.cause.last().map(String::as_str),
            Some(
                "Docker stream error: failed to extract layer: failed to Lchown \"/var/lib/containerd/tmpmounts/m/shadow.mo\": no such file or directory"
            )
        );
    }

    #[test]
    fn a_registry_answer_stays_itself_on_a_full_disk() {
        let error = pull_failure(
            DockerError::DockerResponseServerError {
                status_code: 404,
                message: "not found".into(),
            },
            Some(("/var/lib/docker".into(), 0)),
        );
        assert!(matches!(error, Error::ImagePull(_)));
    }

    fn reason(error: DockerError, free_bytes: u64) -> String {
        let error = pull_failure(error, Some(("/var/lib/docker".into(), free_bytes)));
        ployz_store::Failure::of(
            &ployz_core::ExecutionError::Machine {
                action: ployz_core::MachineAction::PullImage,
                error: ployz_core::RpcError::from(&error),
            },
            &[],
        )
        .reason
    }

    fn streamed(error: &str) -> DockerError {
        DockerError::DockerStreamError {
            error: error.into(),
        }
    }

    #[test]
    fn a_streamed_registry_error_stays_itself_on_a_full_disk() {
        assert_eq!(
            reason(streamed(MANIFEST_UNKNOWN), 48_000_000),
            "pull image failed"
        );
    }

    #[test]
    fn an_unnamed_failure_stays_itself_on_a_full_disk() {
        assert_eq!(
            reason(streamed("unexpected EOF"), 48_000_000),
            "pull image failed"
        );
    }

    #[test]
    fn docker_saying_the_disk_is_out_of_space_names_the_disk() {
        let out_of_space = "write /var/lib/docker/tmp/GetImageBlob1: no space left on device";
        let full = "The Server's disk is full: 48 MB free under /var/lib/docker";
        assert_eq!(reason(streamed(out_of_space), 48_000_000), full);
        assert_eq!(
            reason(
                DockerError::DockerResponseServerError {
                    status_code: 500,
                    message: out_of_space.into(),
                },
                48_000_000
            ),
            full
        );
    }

    #[test]
    fn a_failed_layer_commit_on_a_full_disk_names_the_disk() {
        assert_eq!(
            reason(
                streamed(
                    "failed commit on ref \"layer-sha256:fd04\": commit failed: rename /var/lib/containerd/ingest/fd04/data: no such file or directory"
                ),
                74_000_000
            ),
            "The Server's disk is full: 74 MB free under /var/lib/docker"
        );
    }

    /// A Docker that refuses the pull with a registry error in the pull's stream, as
    /// it does for a missing manifest, and never answers `docker info`.
    async fn runtime_whose_info_never_answers() -> ContainerRuntime {
        use axum::{
            Router,
            http::{Method, StatusCode, Uri},
        };

        let app = Router::new().fallback(|method: Method, uri: Uri| async move {
            if method == Method::POST && uri.path().ends_with("/images/create") {
                (StatusCode::OK, format!("{{\"status\":\"Pulling from acme/missing\"}}\n{{\"errorDetail\":{{\"message\":\"{MANIFEST_UNKNOWN}\"}},\"error\":\"{MANIFEST_UNKNOWN}\"}}\n"))
            } else {
                std::future::pending().await
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        // Bollard's default request timeout, as `LocalDocker::connect` uses.
        let docker = bollard::Docker::connect_with_http(
            &format!("http://{address}"),
            120,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let specs = super::super::MachineSpecStore::open(std::env::temp_dir().join(format!(
            "ployzd-pull-failure-{}.db",
            ployz_core::MachineId::random()
        )))
        .await
        .unwrap();
        ContainerRuntime::new(super::super::LocalDocker::from_client(docker), specs)
    }

    const MANIFEST_UNKNOWN: &str =
        "manifest for ghcr.io/acme/missing:1 not found: manifest unknown: manifest unknown";

    #[tokio::test]
    async fn a_pull_failure_does_not_wait_on_a_stalled_docker_info() {
        let runtime = runtime_whose_info_never_answers().await;
        let started = std::time::Instant::now();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            runtime.pull_image("ghcr.io/acme/missing:1", PullPolicy::Always, None),
        )
        .await
        .expect("the pull failure returns without waiting on docker info")
        .unwrap_err();
        assert!(started.elapsed() < DISK_READ_BOUND + std::time::Duration::from_secs(2));
        assert!(
            matches!(&error, Error::ImagePull(DockerError::DockerStreamError { error }) if error == MANIFEST_UNKNOWN),
            "{error:?}"
        );
    }

    #[test]
    fn docker_refusals_map_to_removal_outcomes() {
        let refused = |status_code| {
            refusal(DockerError::DockerResponseServerError {
                status_code,
                message: "refused".into(),
            })
        };
        assert_eq!(refused(404), ImageRemovalOutcome::NotFound);
        assert_eq!(refused(409), ImageRemovalOutcome::InUse);
        assert!(matches!(refused(500), ImageRemovalOutcome::Failed { .. }));
    }
}
