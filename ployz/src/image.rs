use std::{
    ffi::OsStr,
    future::Future,
    pin::Pin,
    process::{Output, Stdio},
};

use oci_client::Reference;
use ployz_core::{
    EnsureImageIngestRequest, FanoutSelector, ImageIngestDestination, ImageIngestReason,
    ListMachinesRequest, Machine, MachineFailure, MachineId, MachineImages, MachineSuccess,
    MachineTarget, PartialResult, PullImageFromMachineRequest, PullPolicy, RpcError, op,
    resolve_machine_selectors,
};
use thiserror::Error;
use tokio::process::{Child, Command};

use crate::{
    cluster::MachineImagesObservation,
    connect::{Client, rpc_error},
};

use self::proxy::{ImageProxy, ProxyMode, detect_mode};

mod proxy;

#[must_use]
pub fn with_default_tag(image: &str) -> String {
    if image
        .rsplit('/')
        .next()
        .is_some_and(|component| component.contains(':') || component.contains('@'))
    {
        image.to_owned()
    } else {
        format!("{image}:latest")
    }
}

#[derive(Debug, Error)]
pub enum PushError {
    #[error("invalid image reference '{reference}': {message}")]
    InvalidReference { reference: String, message: String },
    #[error("direct image push requires a tagged local reference")]
    DigestReference,
    #[error("direct image push cannot preserve registry-with-port reference '{0}'")]
    RegistryPortReference(String),
    #[error("unsupported platform '{0}'")]
    UnsupportedPlatform(String),
    #[error("image push cancelled")]
    Cancelled,
    #[error("listen for image-push cancellation: {0}")]
    Cancellation(#[source] std::io::Error),
    #[error("image '{0}' not found locally")]
    ImageNotFound(String),
    #[error("Machine target selection failed: {0}")]
    InvalidSelector(#[from] ployz_core::ValueError),
    #[error("Machine target selection failed: {0}")]
    Selector(#[from] ployz_core::MachineSelectorError),
    #[error("Cluster operation failed: {0}")]
    Cluster(#[from] crate::connect::ConnectError),
    #[error("Cluster operation failed: image ingest: {0}")]
    ImageIngest(RpcError),
    #[error("Cluster operation failed: peer image pull: {0}")]
    PeerPull(RpcError),
    #[error("Cluster operation failed: reach unregistry: {0}")]
    Unregistry(crate::connect::ConnectError),
    #[error("Docker {action}: {diagnostic}")]
    Docker {
        action: &'static str,
        diagnostic: String,
    },
    #[error("image proxy {action}: {diagnostic}")]
    Proxy {
        action: &'static str,
        diagnostic: String,
    },
    #[error("Docker is not using the required containerd image store")]
    UnsupportedImageStore,
    #[error("image-push cleanup failed: {0}")]
    Cleanup(String),
    #[error("{primary}; cleanup: {cleanup}")]
    CleanupAfter {
        primary: Box<PushError>,
        cleanup: Box<PushError>,
    },
    #[error("{machine}: {source}")]
    Machine {
        machine: String,
        #[source]
        source: Box<PushError>,
    },
}

impl PushError {
    pub(crate) fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Cancellation(_))
            || matches!(self, Self::CleanupAfter { primary, .. } if primary.is_cancellation())
    }
}

struct Cancellation {
    signal: Pin<Box<dyn Future<Output = std::io::Result<()>>>>,
}

impl Cancellation {
    fn new() -> Self {
        Self {
            signal: Box::pin(tokio::signal::ctrl_c()),
        }
    }

    async fn race<T>(&mut self, future: impl Future<Output = T>) -> Result<T, PushError> {
        tokio::select! {
            output = future => Ok(output),
            result = self.signal.as_mut() => Err(cancellation_error(result)),
        }
    }
}

pub async fn push(
    client: &mut Client,
    image: &str,
    platform: Option<&str>,
    selectors: &[String],
) -> Result<PartialResult<(), PushError>, PushError> {
    let machines = client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await?;
    push_using_machines(client, image, platform, selectors, &machines.machines).await
}

pub(crate) async fn push_using_machines(
    client: &Client,
    image: &str,
    platform: Option<&str>,
    selectors: &[String],
    machines: &[ployz_core::MachineObservation],
) -> Result<PartialResult<(), PushError>, PushError> {
    let mut cancellation = Cancellation::new();
    // TODO(UT-022): without an explicit platform, Docker chooses what to push; target platforms are not inferred.
    let platform = platform.map(validated_platform).transpose()?;
    validate_push_reference(image)?;
    let inspected = cancellation
        .race(docker_output(["image", "inspect", image]))
        .await??;
    if !inspected.status.success() {
        return Err(if not_found(&inspected) {
            PushError::ImageNotFound(image.into())
        } else {
            command_error("inspect local image", &inspected)
        });
    }
    let targets = select_targets(machines, selectors)?;
    let mode = cancellation.race(detect_mode()).await??;
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    let mut source = None;
    for machine in targets {
        let outcome = match source {
            None => push_to_machine(client, image, platform, &machine, mode, &mut cancellation)
                .await
                .map(|destination| {
                    source = Some(destination);
                }),
            Some(destination) => {
                pull_on_machine(client, image, &machine, destination, &mut cancellation).await
            }
        };
        match outcome {
            Ok(()) => result.successes.push(MachineSuccess {
                machine_id: machine.id,
                value: (),
            }),
            Err(error) if error.is_cancellation() => {
                return Err(error);
            }
            Err(error) => result.failures.push(MachineFailure {
                machine_id: machine.id,
                error: PushError::Machine {
                    machine: machine.name.to_string(),
                    source: Box::new(error),
                },
            }),
        }
    }
    Ok(result)
}

pub(crate) struct ImageListSelection {
    pub targets: Vec<Machine>,
    pub omissions: Vec<MachineId>,
}

pub(crate) fn list_selection(
    observations: &[ployz_core::MachineObservation],
    selectors: &[String],
) -> Result<ImageListSelection, PushError> {
    let targets = match select_targets(observations, selectors) {
        Ok(targets) => targets,
        Err(PushError::Selector(ployz_core::MachineSelectorError::NoVisibleMachines)) => Vec::new(),
        Err(error) => return Err(error),
    };
    let omissions = if selectors.is_empty() {
        observations
            .iter()
            .filter(|observation| !observation.membership.invites_rpc())
            .map(|observation| observation.machine.id)
            .collect()
    } else {
        Vec::new()
    };
    Ok(ImageListSelection { targets, omissions })
}

pub(crate) async fn list(
    client: &mut Client,
    reference: Option<String>,
    selectors: &[String],
) -> Result<PartialResult<MachineImagesObservation, RpcError>, PushError> {
    let machines = client.machines().await?;
    let selection = list_selection(&machines, selectors)?;
    if selection.targets.is_empty() {
        return Ok(PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: selection.omissions,
        });
    }
    let mut result = client.list_images(reference, &selection.targets).await;
    result.omissions.extend(selection.omissions);
    Ok(result)
}

pub(crate) fn select_targets(
    observations: &[ployz_core::MachineObservation],
    selectors: &[String],
) -> Result<Vec<Machine>, PushError> {
    let machines = observations
        .iter()
        .filter(|observation| observation.membership.invites_rpc())
        .map(|observation| observation.machine.clone())
        .collect::<Vec<_>>();
    let selectors = if selectors.is_empty() {
        vec![FanoutSelector::All]
    } else {
        selectors
            .iter()
            .map(|selector| FanoutSelector::parse(selector.as_str()))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(resolve_machine_selectors(&machines, &selectors)?)
}

async fn push_to_machine(
    client: &Client,
    image: &str,
    platform: Option<&str>,
    machine: &Machine,
    mode: ProxyMode,
    cancellation: &mut Cancellation,
) -> Result<ImageIngestDestination, PushError> {
    let opened = cancellation
        .race(client.invoke::<op::EnsureImageIngest>(
            EnsureImageIngestRequest {},
            &MachineTarget::from(&machine.id),
            None,
        ))
        .await?
        .map_err(ingest_error)?;
    let remote = format!(
        "{}:{}",
        opened.destination.gateway.0, opened.destination.port
    );
    cancellation
        .race(client.dial_proxy("tcp", &remote))
        .await?
        .map_err(PushError::Unregistry)?;
    PushSession::run(client, remote, mode, image, platform, cancellation).await?;
    Ok(opened.destination)
}

async fn pull_on_machine(
    client: &Client,
    image: &str,
    machine: &Machine,
    source: ImageIngestDestination,
    cancellation: &mut Cancellation,
) -> Result<(), PushError> {
    cancellation
        .race(client.invoke::<op::PullImageFromMachine>(
            PullImageFromMachineRequest {
                image: image.to_owned(),
                source,
            },
            &MachineTarget::from(&machine.id),
            None,
        ))
        .await?
        .map(|_| ())
        .map_err(PushError::PeerPull)
}

/// Pull a missing image from a cluster peer that already has it.
///
/// `Always` leaves the registry pull to the destination Machine. `Missing` and
/// `Never` pull from a peer when one has the image. `Missing` without a peer
/// leaves the registry pull to the destination. `Never` without a peer leaves
/// the destination to fail if the image is absent.
///
/// # Errors
///
/// Returns when listing, opening ingest on the source, or the peer pull fails.
pub(crate) async fn ensure_cluster_image(
    client: &Client,
    dest: &MachineId,
    image: &str,
    policy: PullPolicy,
) -> Result<(), RpcError> {
    match policy {
        PullPolicy::Always => return Ok(()),
        PullPolicy::Missing | PullPolicy::Never => {}
    }
    let mut listing_client = client.clone();
    let machines = listing_client.machines().await.map_err(rpc_error)?;
    let targets = machines
        .into_iter()
        .filter(|machine| machine.membership.invites_rpc())
        .map(|machine| machine.machine)
        .collect::<Vec<_>>();
    let listings = listing_client
        .list_images(Some(image.to_owned()), &targets)
        .await;
    if destination_has_image(dest, &listings.successes, image) {
        return Ok(());
    }
    let Some(peer) = peer_with_image(dest, &listings.successes, image) else {
        return Ok(());
    };
    let opened = client
        .invoke::<op::EnsureImageIngest>(
            EnsureImageIngestRequest {},
            &MachineTarget::from(&peer),
            None,
        )
        .await?;
    client
        .invoke::<op::PullImageFromMachine>(
            PullImageFromMachineRequest {
                image: image.to_owned(),
                source: opened.destination,
            },
            &MachineTarget::from(dest),
            None,
        )
        .await
        .map(|_| ())
}

fn destination_has_image(
    dest: &MachineId,
    listings: &[MachineSuccess<MachineImagesObservation>],
    image: &str,
) -> bool {
    listings
        .iter()
        .any(|success| success.machine_id == *dest && image_present(&success.value.images, image))
}

fn peer_with_image(
    dest: &MachineId,
    listings: &[MachineSuccess<MachineImagesObservation>],
    image: &str,
) -> Option<MachineId> {
    listings.iter().find_map(|success| {
        (success.machine_id != *dest && image_present(&success.value.images, image))
            .then_some(success.machine_id)
    })
}

fn image_present(images: &MachineImages, image: &str) -> bool {
    images.images.iter().any(|summary| {
        summary.repo_tags.iter().any(|tag| {
            tag == image
                || tag
                    .strip_suffix(image)
                    .is_some_and(|prefix| prefix.ends_with('/'))
        })
    })
}

fn ingest_error(error: RpcError) -> PushError {
    match ImageIngestReason::from_details(&error.details) {
        Some(ImageIngestReason::UnsupportedContainerdStore) => PushError::UnsupportedImageStore,
        Some(
            ImageIngestReason::NotParticipating
            | ImageIngestReason::DockerUnavailable
            | ImageIngestReason::ContainerdSocketMissing
            | ImageIngestReason::StartFailed,
        )
        | None => PushError::ImageIngest(error),
    }
}

struct PushSession {
    proxy: ImageProxy,
    temporary: Option<String>,
    command: Option<Child>,
}

impl PushSession {
    async fn run(
        client: &Client,
        remote: String,
        mode: ProxyMode,
        image: &str,
        platform: Option<&str>,
        cancellation: &mut Cancellation,
    ) -> Result<(), PushError> {
        let mut session = Self {
            proxy: ImageProxy::open(mode, cancellation).await?,
            temporary: None,
            command: None,
        };
        let outcome = cancellation
            .race(session.push(client, remote, image, platform))
            .await
            .flatten();
        let cleanup = session.cleanup().await;
        match (outcome, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(PushError::CleanupAfter {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        }
    }

    async fn push(
        &mut self,
        client: &Client,
        remote: String,
        image: &str,
        platform: Option<&str>,
    ) -> Result<(), PushError> {
        let temporary = temporary_reference(self.proxy.push_port(), image);
        self.temporary = Some(temporary.clone());
        self.command = Some(
            Command::new("docker")
                .args(["tag", image, &temporary])
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| PushError::Docker {
                    action: "tag image for push",
                    diagnostic: error.to_string(),
                })?,
        );
        let tagged = self
            .command
            .as_mut()
            .expect("tag command was stored")
            .wait()
            .await
            .map_err(|error| PushError::Docker {
                action: "tag image for push",
                diagnostic: error.to_string(),
            })?;
        if !tagged.success() {
            return Err(PushError::Docker {
                action: "tag image for push",
                diagnostic: format!("exited with {tagged}"),
            });
        }
        let mut command = Command::new("docker");
        command.arg("push");
        if let Some(platform) = platform {
            command.args(["--platform", platform]);
        }
        self.command = Some(command.arg(&temporary).kill_on_drop(true).spawn().map_err(
            |error| PushError::Docker {
                action: "push",
                diagnostic: error.to_string(),
            },
        )?);
        let push = self
            .command
            .as_mut()
            .expect("push command was stored")
            .wait();
        // TODO(UT-023): direct push keeps Docker's progress stream; no quiet mode is exposed.
        tokio::select! {
            outcome = push => {
                let status = outcome.map_err(|error| PushError::Docker {
                    action: "push",
                    diagnostic: error.to_string(),
                })?;
                status.success().then_some(()).ok_or(PushError::Docker {
                    action: "push",
                    diagnostic: format!("exited with {status}"),
                })
            },
            outcome = self.proxy.serve(client.clone(), remote) => outcome,
        }
    }

    async fn cleanup(&mut self) -> Result<(), PushError> {
        let mut errors = Vec::new();
        if let Some(command) = &mut self.command
            && let Err(error) = stop_command(command).await
        {
            errors.push(error.to_string());
        }
        if let Err(error) = self.proxy.cleanup().await {
            errors.push(error.to_string());
        }
        if let Some(temporary) = &self.temporary
            && let Err(error) = remove_image(temporary).await
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(PushError::Cleanup(errors.join("; ")))
        }
    }
}

fn cancellation_error(result: std::io::Result<()>) -> PushError {
    match result {
        Ok(()) => PushError::Cancelled,
        Err(error) => PushError::Cancellation(error),
    }
}

fn validate_push_reference(image: &str) -> Result<(), PushError> {
    let reference = image
        .parse::<Reference>()
        .map_err(|error| PushError::InvalidReference {
            reference: image.into(),
            message: error.to_string(),
        })?;
    if reference.registry().contains(':') {
        return Err(PushError::RegistryPortReference(image.into()));
    }
    if reference.digest().is_some() {
        return Err(PushError::DigestReference);
    }
    Ok(())
}

fn temporary_reference(port: u16, image: &str) -> String {
    format!("127.0.0.1:{port}/{image}")
}

async fn stop_command(command: &mut Child) -> std::io::Result<()> {
    match command.try_wait()? {
        Some(_) => Ok(()),
        None => command.kill().await,
    }
}

fn validated_platform(platform: &str) -> Result<&str, PushError> {
    let components = platform.split('/').collect::<Vec<_>>();
    if matches!(components.len(), 2 | 3)
        && components.iter().all(|component| {
            !component.is_empty()
                && component.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
        })
    {
        Ok(platform)
    } else {
        Err(PushError::UnsupportedPlatform(platform.into()))
    }
}

async fn remove_image(image: &str) -> Result<(), PushError> {
    let output = docker_output(["image", "rm", image]).await?;
    if output.status.success() || not_found(&output) {
        Ok(())
    } else {
        Err(command_error("remove temporary image", &output))
    }
}

async fn docker_output<I, S>(args: I) -> Result<Output, PushError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| PushError::Docker {
            action: "run command",
            diagnostic: error.to_string(),
        })
}

fn command_error(action: &'static str, output: &Output) -> PushError {
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    PushError::Docker {
        action,
        diagnostic: diagnostic.trim().into(),
    }
}

fn not_found(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr)
        .to_ascii_lowercase()
        .contains("no such")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{
        EnsureImageIngestRequest, ImageSummary, ListImagesRequest, MachineId, MachineImages,
        MachineName, MachineObservation, ManagementAddress, MembershipObservation, RpcErrorCode,
        WireGuardPublicKey,
    };
    use serde_json::Value;

    fn machine(seed: u8) -> MachineObservation {
        MachineObservation {
            machine: Machine {
                id: MachineId::parse(format!("{seed:032x}")).unwrap(),
                name: MachineName::parse(format!("machine-{seed}")).unwrap(),
                subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
                management_address: ManagementAddress("fd00::1".parse().unwrap()),
                public_key: WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            membership: MembershipObservation::Up,
            selected_endpoint: None,
            rtt: None,
        }
    }

    #[test]
    fn untagged_image_gains_latest_tag() {
        assert_eq!(with_default_tag("alpine"), "alpine:latest");
    }

    #[test]
    fn tagged_image_stays_as_written() {
        assert_eq!(with_default_tag("alpine:3.20"), "alpine:3.20");
    }

    #[test]
    fn digest_image_stays_as_written() {
        let digest = format!("alpine@sha256:{}", "0".repeat(64));
        assert_eq!(with_default_tag(&digest), digest);
    }

    #[test]
    fn registry_host_port_gains_latest_on_the_name() {
        assert_eq!(
            with_default_tag("localhost:5000/foo"),
            "localhost:5000/foo:latest"
        );
    }

    #[test]
    fn target_and_proxy_selection_preserve_the_explicit_contract() {
        let machines = [machine(1), machine(2)];
        assert_eq!(select_targets(&machines, &[]).unwrap().len(), 2);
        assert_eq!(
            select_targets(&machines, &["machine-2".into()])
                .unwrap()
                .first()
                .unwrap()
                .name
                .as_str(),
            "machine-2"
        );
        assert_eq!(select_targets(&machines, &["*".into()]).unwrap().len(), 2);
        assert!(select_targets(&machines, &["all".into()]).is_err());
        let named_all = MachineObservation {
            machine: Machine {
                name: MachineName::parse("all").unwrap(),
                ..machines[0].machine.clone()
            },
            ..machines[0].clone()
        };
        assert_eq!(
            select_targets(&[named_all.clone(), machines[1].clone()], &["all".into()])
                .unwrap()
                .first()
                .unwrap()
                .name
                .as_str(),
            "all"
        );
        assert!(select_targets(&machines, &["missing".into()]).is_err());
        let mut down = machine(3);
        down.membership = MembershipObservation::Down;
        let mut unknown = machine(4);
        unknown.membership = MembershipObservation::Unknown;
        let mixed = [machine(1), machine(2), down.clone(), unknown];
        assert_eq!(select_targets(&mixed, &[]).unwrap().len(), 2);
        assert!(select_targets(&mixed, &[down.machine.name.to_string()]).is_err());
        let broadcast = list_selection(&mixed, &[]).unwrap();
        assert_eq!(broadcast.targets.len(), 2);
        assert_eq!(broadcast.omissions.len(), 2);
        let named = list_selection(&mixed, &["machine-1".into()]).unwrap();
        assert_eq!(named.targets.len(), 1);
        assert!(named.omissions.is_empty());
        assert_eq!(proxy::mode_for(false, false), ProxyMode::Native);
        assert_eq!(proxy::mode_for(false, true), ProxyMode::Rootless);
        assert_eq!(proxy::mode_for(true, false), ProxyMode::Vm);
        assert_eq!(proxy::mode_for(true, true), ProxyMode::Vm);
        validate_push_reference("registry.test/team/api:v1").unwrap();
        let reference = temporary_reference(5000, "registry.test/team/api:v1");
        assert_eq!(reference, "127.0.0.1:5000/registry.test/team/api:v1");
        assert_eq!(
            temporary_reference(5000, "alpine:3.23"),
            "127.0.0.1:5000/alpine:3.23"
        );
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(matches!(
            validate_push_reference(&format!("registry.test/team/api@{digest}")),
            Err(PushError::DigestReference)
        ));
        assert!(matches!(
            validate_push_reference("localhost:5000/team/api:v1"),
            Err(PushError::RegistryPortReference(_))
        ));
        assert!(validate_push_reference("registry.test/team/api@sha256:abc").is_err());
        assert_eq!(validated_platform("linux/386").unwrap(), "linux/386");
        assert_eq!(validated_platform("linux/arm/v7").unwrap(), "linux/arm/v7");
        assert!(validated_platform("linux").is_err());
        assert!(validated_platform("linux//v7").is_err());
        assert!(
            PushError::CleanupAfter {
                primary: Box::new(PushError::Cancelled),
                cleanup: Box::new(PushError::Cleanup("test cleanup".into())),
            }
            .is_cancellation()
        );
    }

    #[test]
    fn ingest_errors_keep_unsupported_store_distinct() {
        let unsupported = ImageIngestReason::UnsupportedContainerdStore
            .rpc_error("Docker is not using the containerd image store");
        assert!(matches!(
            ingest_error(unsupported),
            PushError::UnsupportedImageStore
        ));
        for reason in [
            ImageIngestReason::NotParticipating,
            ImageIngestReason::DockerUnavailable,
            ImageIngestReason::ContainerdSocketMissing,
            ImageIngestReason::StartFailed,
        ] {
            assert!(matches!(
                ingest_error(reason.rpc_error("ingest unavailable")),
                PushError::ImageIngest(_)
            ));
        }
        assert!(matches!(
            ingest_error(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "ingest unavailable".into(),
                details: Value::Null,
            }),
            PushError::ImageIngest(_)
        ));
    }

    #[test]
    fn image_push_opens_ingest_instead_of_listing_images() {
        let ingest = op::EnsureImageIngest::into_request(EnsureImageIngestRequest {});
        let list = op::ListImages::into_request(ListImagesRequest { reference: None });
        assert_eq!(ingest.body.command(), "ensure_image_ingest");
        assert_eq!(list.body.command(), "list_images");
        assert_ne!(ingest.body.command(), list.body.command());
    }

    fn listing(seed: u8, tags: &[&str]) -> MachineSuccess<MachineImagesObservation> {
        MachineSuccess {
            machine_id: machine(seed).machine.id,
            value: MachineImagesObservation {
                machine_name: MachineName::parse(format!("machine-{seed}")).unwrap(),
                images: MachineImages {
                    containerd_store: true,
                    images: vec![ImageSummary {
                        id: format!("sha256:{seed}"),
                        repo_tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                        created: 0,
                        size: 0,
                        containers: 0,
                        platforms: Vec::new(),
                    }],
                },
            },
        }
    }

    #[test]
    fn missing_image_selects_a_peer_that_already_has_it() {
        let dest = machine(1).machine.id;
        let listings = [
            listing(1, &[]),
            listing(2, &["docker.io/library/busybox:1.37.0"]),
        ];
        assert!(!destination_has_image(&dest, &listings, "busybox:1.37.0"));
        assert_eq!(
            peer_with_image(&dest, &listings, "busybox:1.37.0"),
            Some(machine(2).machine.id)
        );
        assert!(destination_has_image(
            &machine(2).machine.id,
            &listings,
            "busybox:1.37.0"
        ));
        assert_eq!(peer_with_image(&dest, &listings, "missing:tag"), None);
    }
}
