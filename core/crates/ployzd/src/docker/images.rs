//! Images on this Machine: a deploy's registry pull, and Image Cleanup, which finds
//! each repository's superseded build images and removes them unforced, keeping any a
//! Container uses.
//!
//! Only `repository:ployz-sha256-*` tags, which Direct Image Transfer creates, are
//! candidates.

use std::collections::{HashMap, HashSet};

use bollard::{
    errors::Error as DockerError,
    query_parameters::{
        ListContainersOptionsBuilder, ListImagesOptionsBuilder, RemoveImageOptions,
    },
};
use ployz_core::{
    DOCKER_ROOT_NEARLY_FULL_BYTES, DiskFull, DiskSpace, ExposedPort, ImageRemoval,
    ImageRemovalOutcome, ImageSummary, ImagesRemoved, MachineImages, PullPolicy, RETENTION_TAG,
    RegistryAuth,
};

use super::{Error, LocalDocker, Status, classify, containerd_store, project_image};
use crate::docker_image::UnsignedImage;

/// Unused images kept for rollback.
const KEEP: usize = 3;
/// Unused images kept when the Docker root is nearly full.
const KEEP_UNDER_PRESSURE: usize = 1;
/// Beyond [`KEEP`], an unused image goes once this idle.
const IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;
/// Below this free share of the Docker root, keep only [`KEEP_UNDER_PRESSURE`].
const PRESSURE_FREE_PERCENT: u64 = 20;
/// ENOSPC, as Docker words it.
const OUT_OF_SPACE: &str = "no space left on device";
/// The steps in which containerd writes a pulled layer to disk.
const LAYER_WRITES: [&str; 3] = [
    "failed to extract layer",
    "failed to register layer",
    "failed commit on ref",
];
/// Where containerd keeps the image store Ployz has Docker use.
const CONTAINERD_ROOT: &str = "/var/lib/containerd";
/// How long a failed pull waits on `docker info` before reporting the failure as is.
const DISK_READ_BOUND: std::time::Duration = std::time::Duration::from_secs(3);

/// What Docker holds as images on this Machine: pulling, listing and Image Cleanup.
#[derive(Clone)]
pub struct Images {
    docker: LocalDocker,
    free: std::sync::Arc<std::sync::Mutex<Free>>,
}

/// What this Machine pulled itself, from a public registry with no login, since this
/// daemon started. Only [`Images::pull_free`] writes it, and only unsigned creations
/// read it, in place of a Docker tag a caller with Full access may have written.
#[derive(Default)]
struct Free {
    ids: HashSet<String>,
    /// The image each reference, spelled as [`ployz_core::whole_image_name`] spells it,
    /// last pulled to.
    named: HashMap<String, String>,
}

impl Images {
    pub(super) fn new(docker: LocalDocker) -> Self {
        Self {
            docker,
            free: std::sync::Arc::default(),
        }
    }

    /// Make `image` one this Machine pulled itself with no login, for a Container of a
    /// spec nothing signed, and answer its ID. The image this Machine last pulled so for
    /// `image` comes first, then one here under that name that such a pull placed, so a
    /// private image is never run for a caller who only knows its name. `policy` pulls
    /// again under `Always`, and under `Never` runs only what is here. Writes no name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsignedImage`] when the registry is not public, the pull fails,
    /// or `policy` is `Never` and no such pull placed the image, and [`Error::DiskFull`]
    /// when the pull fails on a full disk.
    pub(crate) async fn pull_free(&self, image: &str, policy: PullPolicy) -> Result<String, Error> {
        let docker = &self.docker.client;
        let key = ployz_core::whole_image_name(image).unwrap_or_else(|| image.to_owned());
        if policy != PullPolicy::Always {
            let pulled = self.free_ids().named.get(&key).cloned();
            for reference in pulled.iter().map(String::as_str).chain([image]) {
                match docker.inspect_image(reference).await {
                    Ok(local) => {
                        if let Some(id) = local.id.filter(|id| self.is_free(id)) {
                            return Ok(id);
                        }
                    }
                    Err(error) if crate::docker_image::is_not_found(&error) => {}
                    Err(error) => return Err(Error::Docker(error)),
                }
            }
        }
        if policy == PullPolicy::Never {
            return Err(Error::UnsignedImage(UnsignedImage::NeverPulled));
        }
        match crate::docker_image::pull_public(docker, image).await {
            Ok(id) => {
                let mut free = self.free_ids();
                free.ids.insert(id.clone());
                free.named.insert(key, id.clone());
                Ok(id)
            }
            Err(UnsignedImage::Pull(error)) => {
                let failure = self.pull_failure(Error::ImagePull(error)).await;
                Err(if let Error::ImagePull(error) = failure {
                    Error::UnsignedImage(UnsignedImage::Pull(error))
                } else {
                    failure
                })
            }
            Err(refusal) => Err(Error::UnsignedImage(refusal)),
        }
    }

    /// Whether `image_id` is an image [`Self::pull_free`] placed.
    pub(crate) fn is_free(&self, image_id: &str) -> bool {
        self.free_ids().ids.contains(image_id)
    }

    fn free_ids(&self) -> std::sync::MutexGuard<'_, Free> {
        self.free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
            return Err(self.pull_failure(Error::ImagePull(error)).await);
        }
        self.exposed_ports(image).await
    }

    /// Make `image` present as `policy` asks, with no login, and answer the ID it names
    /// here now, so a creation can run that ID whatever the tag names later.
    ///
    /// # Errors
    ///
    /// As [`Self::pull_image`], [`Error::ImageAbsent`] when `policy` pulls nothing and
    /// the image isn't here, and when Docker names no ID for it.
    pub(crate) async fn place(&self, image: &str, policy: PullPolicy) -> Result<String, Error> {
        let docker = &self.docker.client;
        if let Err(error) = crate::docker_image::prepare_image(docker, image, policy, None).await {
            return Err(self.pull_failure(Error::ImagePull(error)).await);
        }
        match docker.inspect_image(image).await {
            Ok(inspected) => inspected.id.ok_or(Error::MissingField("image ID")),
            Err(error)
                if policy == PullPolicy::Never && crate::docker_image::is_not_found(&error) =>
            {
                Err(Error::ImageAbsent(image.to_owned()))
            }
            Err(error) => Err(Error::ImagePull(error)),
        }
    }

    /// [`Self::pull_free`] for a Deploy's pull phase: the ports the image it found exposes.
    ///
    /// # Errors
    ///
    /// As [`Self::pull_free`], and when Docker no longer holds the image it found.
    pub async fn pull_free_image(
        &self,
        image: &str,
        policy: PullPolicy,
    ) -> Result<Vec<ExposedPort>, Error> {
        let id = self.pull_free(image, policy).await?;
        self.exposed_ports(&id).await
    }

    async fn exposed_ports(&self, reference: &str) -> Result<Vec<ExposedPort>, Error> {
        let inspect = self
            .docker
            .client
            .inspect_image(reference)
            .await
            .map_err(Error::ImagePull)?;
        Ok(exposed_ports(
            inspect
                .config
                .and_then(|config| config.exposed_ports)
                .unwrap_or_default(),
        ))
    }

    /// `error` from a registry or peer pull, as a full disk when the Docker root, or
    /// containerd's store where Docker keeps images, has almost no free space left:
    /// containerd then fails mid-extract with whatever call ran out. A directory it
    /// can't measure, such as a Docker root `docker info` doesn't answer within
    /// [`DISK_READ_BOUND`], is left out; with none, `error` stays as it is.
    pub(crate) async fn pull_failure(&self, error: Error) -> Error {
        if disk_sign(&error).is_none() {
            return error;
        }
        let space = |dir: String| {
            let (_, free_bytes) = crate::host_capacity::filesystem_space(&dir).ok()?;
            Some((dir, free_bytes))
        };
        let docker_root = tokio::time::timeout(DISK_READ_BOUND, self.docker.client.info())
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|info| info.docker_root_dir)
            .and_then(space);
        let stores: Vec<_> = space(CONTAINERD_ROOT.to_owned()).into_iter().collect();
        pull_failure(error, docker_root, &stores)
    }

    pub async fn list_images(
        &self,
        reference: Option<&str>,
        last_tagged: bool,
    ) -> Result<MachineImages, Error> {
        let filters = reference
            .map(|reference| HashMap::from([("reference", vec![reference])]))
            .unwrap_or_default();
        let options = ListImagesOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .manifests(true)
            .build();
        let listed = self.docker.client.list_images(Some(options)).await?;
        let tagged = if last_tagged {
            futures_util::future::try_join_all(
                listed.iter().map(|image| self.last_tagged(&image.id)),
            )
            .await?
        } else {
            vec![None; listed.len()]
        };
        let images = listed
            .into_iter()
            .zip(tagged)
            .map(|(image, last_tagged)| ImageSummary {
                last_tagged,
                ..project_image(image)
            })
            .collect();
        let info = self.docker.client.info().await?;
        Ok(MachineImages {
            containerd_store: info.driver_status.as_deref().is_some_and(containerd_store),
            images,
            docker_root: info
                .docker_root_dir
                .and_then(|root| crate::host_capacity::filesystem_space(&root).ok())
                .map(|(total_bytes, free_bytes)| DiskSpace {
                    total_bytes,
                    free_bytes,
                }),
        })
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
            Err(error) if classify(&error) == Status::Absent => Ok(None),
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

/// What in a failed pull's output points at a full disk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiskSign {
    /// Docker said the device is out of space.
    OutOfSpace,
    /// containerd failed writing a registry layer, which a full disk explains only
    /// when the Docker root has almost no free space left.
    LayerWrite,
}

/// The sign of a full disk in a failed pull, and what the pull printed; none for an error no full disk
/// explains, a registry's not-found answer among them, whatever its text says, so
/// the CLI names the missing image. A peer pull cut short also fails its layer
/// commit, so there only ENOSPC counts.
fn disk_sign(error: &Error) -> Option<(DiskSign, String)> {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "only a failed pull can be a full disk"
    )]
    let (message, layer_rule) = match error {
        Error::ImagePull(docker) if !crate::docker_image::is_not_found(docker) => {
            (docker.to_string(), true)
        }
        Error::PeerPull(printed) => (printed.clone(), false),
        _ => return None,
    };
    let sign = if message.contains(OUT_OF_SPACE) {
        DiskSign::OutOfSpace
    } else if layer_rule && LAYER_WRITES.iter().any(|write| message.contains(write)) {
        DiskSign::LayerWrite
    } else {
        return None;
    };
    Some((sign, message))
}

/// A registry or peer pull is the full disk, with Docker's error as its cause, when
/// Docker says the device is out of space, or when containerd failed writing a
/// registry layer with under [`DOCKER_ROOT_NEARLY_FULL_BYTES`] free: it then fails with
/// whatever call ran out. Each space is a directory and its free bytes. The disk
/// named is the fullest of `stores` the error wrote under, else `docker_root`; a
/// store it never wrote under may not be the one in use. Any other failure stays
/// itself.
fn pull_failure(
    error: Error,
    docker_root: Option<(String, u64)>,
    stores: &[(String, u64)],
) -> Error {
    let Some((sign, message)) = disk_sign(&error) else {
        return error;
    };
    let written = stores
        .iter()
        .filter(|(dir, _)| writes_under(&message, dir))
        .min_by_key(|(_, free_bytes)| *free_bytes)
        .cloned();
    match written.or(docker_root) {
        Some((dir, free_bytes))
            if sign == DiskSign::OutOfSpace || free_bytes < DOCKER_ROOT_NEARLY_FULL_BYTES =>
        {
            Error::DiskFull {
                full: DiskFull {
                    docker_root: dir,
                    free_bytes,
                },
                source: if let Error::ImagePull(docker) = error {
                    Box::new(docker)
                } else {
                    Box::new(error)
                },
            }
        }
        _ => error,
    }
}

/// Whether `message` names a path under `dir`: `dir/` where a path starts, so that
/// `/mnt/data/var/lib/containerd/…` is not under `/var/lib/containerd`.
fn writes_under(message: &str, dir: &str) -> bool {
    let needle = format!("{dir}/");
    message.match_indices(&needle).any(|(at, _)| {
        message[..at]
            .chars()
            .next_back()
            .is_none_or(|before| before.is_whitespace() || matches!(before, '"' | '\'' | '(' | '='))
    })
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
    match classify(&error) {
        Status::Absent => ImageRemovalOutcome::NotFound,
        Status::Conflict => ImageRemovalOutcome::InUse,
        Status::Unchanged | Status::Other => ImageRemovalOutcome::Failed {
            message: ployz_core::error_chain::inline(&error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinned_image_is_kept_under_its_repository_whatever_tag_the_pin_names() {
        let hex = "1".repeat(64);
        for (image, kept) in [
            (
                format!("redis:7-alpine@sha256:{hex}"),
                format!("redis:ployz-sha256-{hex}"),
            ),
            (
                format!("example.test/api@sha256:{hex}"),
                format!("example.test/api:ployz-sha256-{hex}"),
            ),
            (
                format!("localhost:5000/api@sha256:{hex}"),
                format!("localhost:5000/api:ployz-sha256-{hex}"),
            ),
            (
                format!("localhost:5000/api:v1@sha256:{hex}"),
                format!("localhost:5000/api:ployz-sha256-{hex}"),
            ),
        ] {
            let tag = ployz_core::retention_tag(&image).unwrap();
            assert_eq!(tag, kept);
            assert!(ployz_core::is_image_reference(&tag), "{tag}");
        }
    }

    #[test]
    fn the_retention_tag_text_is_the_one_other_daemons_read() {
        assert_eq!(RETENTION_TAG, ":ployz-sha256-");
    }

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
    fn failed_pull(free_bytes: u64) -> ployz_core::deploy_progress::Failure {
        let lchown = DockerError::DockerStreamError {
            error: "failed to extract layer: failed to Lchown \"/var/lib/containerd/tmpmounts/m/shadow.mo\": no such file or directory".into(),
        };
        let error = pull_failure(
            Error::ImagePull(lchown),
            Some(("/var/lib/docker".into(), free_bytes)),
            &[],
        );
        ployz_core::deploy_progress::Failure::of(
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

    /// What a peer pull onto a full disk printed on Incus (#2875).
    const PEER_EXTRACT: &str = "failed to extract layer (application/vnd.oci.image.layer.v1.tar+gzip sha256:759e) to overlayfs as \"extract-570279893-IhHy sha256:4105\": mount callback failed on /var/lib/containerd/tmpmounts/containerd-mount82441223: write /var/lib/containerd/tmpmounts/containerd-mount82441223/usr/lib/libcrypto.so.3: no space left on device";

    /// The deploy output for a peer pull that failed with `printed`.
    fn failed_peer_pull(printed: &str, free_bytes: u64) -> ployz_core::deploy_progress::Failure {
        let error = pull_failure(
            Error::PeerPull(printed.into()),
            Some(("/var/lib/docker".into(), free_bytes)),
            &[],
        );
        ployz_core::deploy_progress::Failure::of(
            &ployz_core::ExecutionError::Machine {
                action: ployz_core::MachineAction::PullImage,
                error: ployz_core::RpcError::from(&error),
            },
            &[],
        )
    }

    #[test]
    fn a_peer_pull_on_a_full_disk_names_the_disk_and_keeps_its_error() {
        let failure = failed_peer_pull(PEER_EXTRACT, 0);
        assert_eq!(
            failure.reason,
            "The Server's disk is full: 0 MB free under /var/lib/docker"
        );
        assert_eq!(
            failure.cause,
            [format!("peer image pull failed: {PEER_EXTRACT}")]
        );
    }

    #[test]
    fn a_peer_pull_that_fails_otherwise_stays_itself() {
        let failure = failed_peer_pull("manifest unknown", 0);
        assert_eq!(failure.reason, "pull image failed");
        assert_eq!(failure.cause, ["peer image pull failed: manifest unknown"]);
    }

    /// A peer's stream cut short fails the layer commit on a nearly full disk too:
    /// only ENOSPC names the disk for a peer pull.
    #[test]
    fn a_peer_pull_cut_short_on_a_nearly_full_disk_stays_itself() {
        let cut =
            "failed commit on ref \"layer-sha256:759e\": commit failed: unexpected commit digest";
        let failure = failed_peer_pull(cut, 800_000_000);
        assert_eq!(failure.reason, "pull image failed");
        assert_eq!(failure.cause, [format!("peer image pull failed: {cut}")]);
    }

    #[test]
    fn a_peer_pull_on_a_full_disk_answers_the_disk_it_ran_out_on() {
        let error = pull_failure(
            Error::PeerPull(PEER_EXTRACT.into()),
            Some(("/var/lib/docker".into(), 0)),
            &[],
        );
        let answered = ployz_core::RpcError::from(&error);
        assert_eq!(answered.code, ployz_core::RpcErrorCode::Internal);
        assert_eq!(
            answered.disk_full(),
            Some(DiskFull {
                docker_root: "/var/lib/docker".into(),
                free_bytes: 0,
            })
        );
    }

    /// Docker keeps images in containerd's store, which is often on another disk
    /// than the Docker root (#3155).
    #[test]
    fn a_pull_names_the_disk_that_ran_out_not_the_docker_roots() {
        let error = pull_failure(
            Error::ImagePull(streamed(
                "failed to copy: failed to send write: write /var/lib/containerd/io.containerd.content.v1.content/ingest/ab/data: no space left on device",
            )),
            Some(("/var/lib/docker".into(), 39_789_000_000)),
            &[("/var/lib/containerd".into(), 0)],
        );
        assert_eq!(
            ployz_core::RpcError::from(&error).disk_full(),
            Some(DiskFull {
                docker_root: "/var/lib/containerd".into(),
                free_bytes: 0,
            })
        );
    }

    /// containerd may keep its store elsewhere: a low `/var/lib/containerd` that the
    /// error never wrote under takes no blame.
    #[test]
    fn a_layer_write_names_no_disk_the_pull_did_not_write_to() {
        let error = pull_failure(
            Error::ImagePull(streamed(
                "failed to extract layer: failed to Lchown \"/srv/containerd/tmpmounts/m/shadow.mo\": no such file or directory",
            )),
            Some(("/var/lib/docker".into(), 50_000_000_000)),
            &[("/var/lib/containerd".into(), 500_000_000)],
        );
        assert!(matches!(error, Error::ImagePull(_)), "{error:?}");
    }

    #[test]
    fn a_store_moved_under_another_directory_is_not_the_default_one() {
        let error = pull_failure(
            Error::ImagePull(streamed(
                "failed to extract layer: failed to Lchown \"/mnt/data/var/lib/containerd/tmpmounts/m/shadow.mo\": no such file or directory",
            )),
            Some(("/var/lib/docker".into(), 50_000_000_000)),
            &[("/var/lib/containerd".into(), 500_000_000)],
        );
        assert!(matches!(error, Error::ImagePull(_)), "{error:?}");
    }

    #[test]
    fn a_layer_write_under_a_nearly_full_containerd_store_names_it() {
        let error = pull_failure(
            Error::ImagePull(streamed(
                "failed to extract layer: failed to Lchown \"/var/lib/containerd/tmpmounts/m/shadow.mo\": no such file or directory",
            )),
            Some(("/var/lib/docker".into(), 50_000_000_000)),
            &[("/var/lib/containerd".into(), 500_000_000)],
        );
        assert_eq!(
            ployz_core::RpcError::from(&error).disk_full(),
            Some(DiskFull {
                docker_root: "/var/lib/containerd".into(),
                free_bytes: 500_000_000,
            })
        );
    }

    #[test]
    fn a_layer_write_is_a_full_disk_only_under_one_billion_free_bytes() {
        let write = "failed to extract layer: failed to Lchown \"/var/lib/containerd/tmpmounts/m/shadow.mo\": no such file or directory";
        for (free_bytes, full) in [(999_999_999, true), (1_000_000_000, false)] {
            let error = pull_failure(
                Error::ImagePull(streamed(write)),
                Some(("/var/lib/docker".into(), free_bytes)),
                &[],
            );
            assert_eq!(
                matches!(error, Error::DiskFull { .. }),
                full,
                "{free_bytes}"
            );
        }
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
    fn a_missing_image_stays_missing_and_out_of_space_stays_a_full_disk() {
        let both = format!("{MANIFEST_UNKNOWN}: no space left on device");
        assert_eq!(
            reason(
                DockerError::DockerResponseServerError {
                    status_code: 404,
                    message: both,
                },
                0
            ),
            "Image ghcr.io/acme/missing:1 not found, or its registry needs credentials"
        );
        assert_eq!(
            reason(
                DockerError::DockerResponseServerError {
                    status_code: 500,
                    message: "write /var/lib/docker/tmp/GetImageBlob1: no space left on device"
                        .into(),
                },
                48_000_000
            ),
            "The Server's disk is full: 48 MB free under /var/lib/docker"
        );
    }

    /// The reason a deploy shows for `error`, carried as the CLI carries a failed pull.
    fn reason(error: DockerError, free_bytes: u64) -> String {
        let error = pull_failure(
            Error::ImagePull(error),
            Some(("/var/lib/docker".into(), free_bytes)),
            &[],
        );
        ployz_core::deploy_progress::Failure::of(
            &ployz_core::ExecutionError::Machine {
                action: ployz_core::MachineAction::PullImage,
                error: ployz_core::RpcError::from(&error)
                    .pulling("ghcr.io/acme/missing:1", PullPolicy::Always),
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
    async fn images_whose_info_never_answers() -> Images {
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
        Images::new(LocalDocker::from_client(docker))
    }

    const MANIFEST_UNKNOWN: &str =
        "manifest for ghcr.io/acme/missing:1 not found: manifest unknown: manifest unknown";

    #[tokio::test]
    async fn a_pull_failure_does_not_wait_on_a_stalled_docker_info() {
        let images = images_whose_info_never_answers().await;
        let started = std::time::Instant::now();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            images.pull_image("ghcr.io/acme/missing:1", PullPolicy::Always, None),
        )
        .await
        .expect("the pull failure returns without waiting on docker info")
        .unwrap_err();
        // No full disk explains a missing manifest, so it never asks Docker at all.
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(
            matches!(&error, Error::ImagePull(DockerError::DockerStreamError { error }) if error == MANIFEST_UNKNOWN),
            "{error:?}"
        );
    }

    /// An unsigned `never` redeploy runs what this Machine last pulled for the name, spelled
    /// either way, while the registry now serves something else and a Full build holds the
    /// tag. No pull of the name writes it.
    #[tokio::test]
    async fn a_free_pull_is_found_again_by_any_spelling_and_names_nothing() {
        let (runtime, fake) =
            crate::docker::test_support::fake_runtime_with(Default::default()).await;
        let images = runtime.images();
        let served = format!("sha256:{}", "e".repeat(64));
        fake.registry
            .lock()
            .unwrap()
            .insert("nginx:latest".into(), served.clone());
        let pulled = images
            .pull_free("nginx", PullPolicy::Missing)
            .await
            .unwrap();
        assert_eq!(pulled, served);
        assert_eq!(fake.images.lock().unwrap().get("nginx:latest"), None);

        let built = format!("sha256:{}", "b".repeat(64));
        fake.images
            .lock()
            .unwrap()
            .insert("library/nginx:latest".into(), Some(built));
        fake.registry
            .lock()
            .unwrap()
            .insert("nginx:latest".into(), format!("sha256:{}", "f".repeat(64)));
        let again = images
            .pull_free("library/nginx:latest", PullPolicy::Never)
            .await
            .unwrap();
        assert_eq!(again, served);
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
