use std::{
    ffi::OsStr,
    future::Future,
    process::{Output, Stdio},
};

use oci_client::Reference;
use ployz_core::{
    EnsureImageIngestRequest, FanoutSelector, ImageIngestDestination, ImageIngestReason,
    ListMachinesRequest, Machine, MachineFailure, MachineId, MachineImages, MachineSuccess,
    MachineTarget, PartialResult, PeerImagePull, PullImageFromMachineRequest, PullPolicy, RpcError,
    op, resolve_machine_selectors,
};
use thiserror::Error;
use tokio::process::{Child, Command};

use crate::{
    cluster::MachineImagesObservation,
    connect::{Client, UNARY_RETRY_DELAYS, rpc_error},
};

use self::proxy::{ImageProxy, ProxyMode, detect_mode};

mod built;
mod proxy;
use built::Source;
pub use built::push_from_machine;
pub(crate) use built::{
    available_variant, platform_compatible, push_from_machine_using_machines, serve_build_image,
};

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
    #[error(
        "Machine {machine_id} does not hold {image} content for {platform}; a tag or image index alone is not deliverable content"
    )]
    VariantUnavailable {
        image: String,
        machine_id: MachineId,
        platform: String,
    },
    #[error(
        "Machine {machine_id} holds Build image {image} without {}; a partial Build host cannot be the source",
        .missing.join(", ")
    )]
    BuildIncomplete {
        image: String,
        machine_id: MachineId,
        missing: Vec<String>,
    },
    #[error("invalid image reference '{reference}': {message}")]
    InvalidReference { reference: String, message: String },
    #[error("direct image push requires a tagged local reference")]
    DigestReference,
    #[error(
        "direct image push cannot preserve registry-with-port reference '{0}'; retag the image without a registry port (for example, api:v1), then push that tag"
    )]
    RegistryPortReference(String),
    #[error(
        "unsupported platform '{0}'; use os/arch[/variant] with lowercase components, for example linux/amd64 or linux/arm/v7"
    )]
    UnsupportedPlatform(String),
    #[error("image push cancelled")]
    Cancelled,
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
    #[error(
        "Docker on the target Machine is not using the required containerd image store; enable Docker's containerd image store on that Machine before retrying image push"
    )]
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
        matches!(self, Self::Cancelled)
            || matches!(self, Self::CleanupAfter { primary, .. } if primary.is_cancellation())
    }
}

struct Cancellation<'token> {
    token: &'token tokio_util::sync::CancellationToken,
}

impl<'token> Cancellation<'token> {
    fn new(token: &'token tokio_util::sync::CancellationToken) -> Self {
        Self { token }
    }

    async fn race<T>(&mut self, future: impl Future<Output = T>) -> Result<T, PushError> {
        tokio::select! {
            biased;
            () = self.token.cancelled() => Err(PushError::Cancelled),
            output = future => Ok(output),
        }
    }
}

/// What to transfer: the reference to publish, and the exact content it holds.
///
/// A Build binds the two separately so a later Build moving the same tag
/// cannot substitute its own image during delivery.
#[derive(Clone, Copy, Debug)]
pub enum ImageContent<'a> {
    /// Follow the content currently named by this tag.
    Tagged(&'a str),
    /// Keep the content fixed while publishing the requested reference.
    Pinned { published: &'a str, exact: &'a str },
}

impl<'a> ImageContent<'a> {
    /// A reference carrying whatever content its tag resolves to now.
    #[must_use]
    pub fn tagged(image: &'a str) -> Self {
        Self::Tagged(image)
    }

    /// A published tag bound to exact content, such as a Build's result.
    #[must_use]
    pub fn built(published: &'a str, exact: &'a str) -> Self {
        Self::Pinned { published, exact }
    }

    fn published(self) -> &'a str {
        match self {
            Self::Tagged(image)
            | Self::Pinned {
                published: image, ..
            } => image,
        }
    }

    fn exact(self) -> &'a str {
        match self {
            Self::Tagged(image) | Self::Pinned { exact: image, .. } => image,
        }
    }
}

pub async fn push(
    client: &mut Client,
    content: ImageContent<'_>,
    platform: Option<&str>,
    selectors: &[String],
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<PartialResult<(), PushError>, PushError> {
    let machines = Cancellation::new(cancellation)
        .race(client.call::<op::ListMachines>(ListMachinesRequest {}, None))
        .await??;
    push_using_machines(
        client,
        content,
        platform,
        selectors,
        &machines.machines,
        cancellation,
    )
    .await
}

pub(crate) async fn push_using_machines(
    client: &mut Client,
    content: ImageContent<'_>,
    platform: Option<&str>,
    selectors: &[String],
    machines: &[ployz_core::MachineObservation],
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<PartialResult<(), PushError>, PushError> {
    let mut cancellation = Cancellation::new(cancellation);
    // Without an explicit platform Docker pushes every variant it holds; each
    // destination then receives the one its architecture runs.
    let platform = platform.map(validated_platform).transpose()?;
    let image = content.published();
    validate_push_reference(image)?;
    let inspected = cancellation
        .race(docker_output(["image", "inspect", content.exact()]))
        .await??;
    if !inspected.status.success() {
        return Err(if not_found(&inspected) {
            PushError::ImageNotFound(content.exact().into())
        } else {
            command_error("inspect local image", &inspected)
        });
    }
    let mut targets = select_targets(machines, selectors)?.into_iter();
    let mode = cancellation.race(detect_mode()).await??;
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    // Docker pushes the whole image to the first reachable target, which then
    // serves its peers. Read back what that target actually holds before any
    // peer relies on it; a target that received the image but cannot be read
    // keeps its success and the next target is pushed to instead.
    let mut source = None;
    for machine in targets.by_ref() {
        let pushed =
            push_to_machine(client, content, platform, &machine, mode, &mut cancellation).await;
        let delivered = pushed.is_ok();
        if record(&mut result, &machine, pushed.map(|_| ())).is_err() {
            break;
        }
        if !delivered {
            continue;
        }
        match Source::open(client, machine.id, &mut cancellation).await {
            Ok(opened) => {
                source = Some(opened);
                break;
            }
            Err(error) if error.is_cancellation() => break,
            // The image arrived, so the success stands; say why this Machine
            // will not serve its peers rather than let a second push look odd.
            Err(error) => eprintln!(
                "WARNING: Machine {} received {image} but cannot serve it to peers ({error}); pushing to the next Machine",
                machine.name
            ),
        }
    }
    let Some(source) = source else {
        result.omissions.extend(targets.map(|machine| machine.id));
        return Ok(result);
    };
    for machine in targets.by_ref() {
        let outcome = source
            .deliver(client, content, &machine, platform, &mut cancellation)
            .await;
        if record(&mut result, &machine, outcome).is_err() {
            break;
        }
    }
    result.omissions.extend(targets.map(|machine| machine.id));
    Ok(result)
}

/// Keep one Machine's outcome in the partial result; cancellation ends the fan-out.
fn record(
    result: &mut PartialResult<(), PushError>,
    machine: &Machine,
    outcome: Result<(), PushError>,
) -> Result<(), PushError> {
    match outcome {
        Ok(()) => result.successes.push(MachineSuccess {
            machine_id: machine.id,
            value: (),
        }),
        Err(error) if error.is_cancellation() => {
            result.omissions.push(machine.id);
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
    Ok(())
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
    client: &mut Client,
    content: ImageContent<'_>,
    platform: Option<&str>,
    machine: &Machine,
    mode: ProxyMode,
    cancellation: &mut Cancellation<'_>,
) -> Result<ImageIngestDestination, PushError> {
    // EnsureImageIngest is idempotent; a dropped RPC must not fail the Machine.
    let opened = cancellation
        .race(client.call::<op::EnsureImageIngest>(
            EnsureImageIngestRequest {},
            Some(&MachineTarget::from(&machine.id)),
        ))
        .await?
        .map_err(|error| ingest_error(rpc_error(error)))?;
    let remote = format!(
        "[{}]:{}",
        opened.destination.management_address.0, opened.destination.port
    );
    cancellation
        .race(proxy::dial_with_retry(client, &remote))
        .await?
        .map_err(PushError::Unregistry)?;
    PushSession::run(client, remote, mode, content, platform, cancellation).await?;
    Ok(opened.destination)
}

async fn pull_on_machine(
    client: &mut Client,
    content: ImageContent<'_>,
    machine: &Machine,
    source: ImageIngestDestination,
    platform: &str,
    cancellation: &mut Cancellation<'_>,
) -> Result<(), PushError> {
    let pull = match content {
        ImageContent::Tagged(image) => PeerImagePull::Reference {
            image: image.to_owned(),
        },
        ImageContent::Pinned { published, exact } => {
            let reference =
                published
                    .parse::<Reference>()
                    .map_err(|error| PushError::InvalidReference {
                        reference: published.to_owned(),
                        message: error.to_string(),
                    })?;
            // Upload uses the raw repository path, including short names.
            // Reference's normalized repository would address a different path.
            let repository = if reference.digest().is_some() {
                published
                    .rsplit_once('@')
                    .expect("parsed digest reference")
                    .0
            } else {
                reference
                    .tag()
                    .and_then(|tag| published.strip_suffix(tag)?.strip_suffix(':'))
                    .unwrap_or(published)
            };
            let digest = exact.rsplit_once('@').map_or(exact, |(_, digest)| digest);
            let image = format!("{repository}@{digest}");
            if reference.digest().is_some() {
                PeerImagePull::Reference { image }
            } else {
                PeerImagePull::Publish {
                    image: ployz_core::ImageDigestReference::parse(&image).map_err(|error| {
                        PushError::InvalidReference {
                            reference: image,
                            message: error.to_string(),
                        }
                    })?,
                    tag: published.to_owned(),
                }
            }
        }
    };
    cancellation
        .race(client.call::<op::PullImageFromMachine>(
            PullImageFromMachineRequest {
                pull,
                source,
                platform: platform.to_owned(),
            },
            Some(&MachineTarget::from(&machine.id)),
        ))
        .await?
        .map(|_| ())
        .map_err(|error| PushError::PeerPull(rpc_error(error)))
}

/// Pull a missing image from a cluster peer that demonstrably holds it.
///
/// `Always` leaves the registry pull to the destination Machine. `Missing` and
/// `Never` pull from a peer whose store holds the variant the destination's
/// architecture runs, selected by [`available_variant`]. `Missing` without such
/// a peer leaves the registry pull to the destination. `Never` without one
/// leaves the destination to fail if the image is absent.
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
    // The Deploy plan chose `dest` from an earlier observation. If this one no
    // longer shows it, nothing here can name its platform; the destination's
    // own pull policy decides, as it does when no peer holds the image.
    let Some(architecture) = machines
        .iter()
        .find(|machine| machine.machine.id == *dest)
        .map(|machine| machine.machine.runtime.architecture.clone())
    else {
        return Ok(());
    };
    let targets = machines
        .into_iter()
        .filter(|machine| machine.membership.invites_rpc())
        .map(|machine| machine.machine)
        .collect::<Vec<_>>();
    // Docker's reference filter does not match repository@digest.
    let filter = (!image.contains('@')).then(|| image.to_owned());
    let listings = listing_client.list_images(filter, &targets).await;
    let holders = listings
        .successes
        .iter()
        .filter_map(|success| {
            available_variant(&success.value.images, image, &architecture)
                .map(|platform| (success.machine_id, platform))
        })
        .collect::<Vec<_>>();
    if holders.iter().any(|(machine_id, _)| machine_id == dest) {
        return Ok(());
    }
    let Some((peer, platform)) = holders.iter().find(|(machine_id, _)| machine_id != dest) else {
        return Ok(());
    };
    let opened = listing_client
        .call::<op::EnsureImageIngest>(
            EnsureImageIngestRequest {},
            Some(&MachineTarget::from(peer)),
        )
        .await
        .map_err(rpc_error)?;
    listing_client
        .call::<op::PullImageFromMachine>(
            PullImageFromMachineRequest {
                pull: PeerImagePull::Reference {
                    image: image.to_owned(),
                },
                source: opened.destination,
                platform: (*platform).to_owned(),
            },
            Some(&MachineTarget::from(dest)),
        )
        .await
        .map(|_| ())
        .map_err(rpc_error)
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
        content: ImageContent<'_>,
        platform: Option<&str>,
        cancellation: &mut Cancellation<'_>,
    ) -> Result<(), PushError> {
        let mut session = Self {
            proxy: ImageProxy::open(mode, cancellation).await?,
            temporary: None,
            command: None,
        };
        let outcome = cancellation
            .race(session.push(client, remote, content, platform))
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
        content: ImageContent<'_>,
        platform: Option<&str>,
    ) -> Result<(), PushError> {
        let temporary = temporary_reference(self.proxy.push_port(), content.published());
        self.temporary = Some(temporary.clone());
        self.command = Some(
            Command::new("docker")
                // Tag the content this attempt built, under the published reference.
                .args(["tag", content.exact(), &temporary])
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
        // A dropped Machine tunnel fails `docker push`; another attempt reuses
        // layers already on unregistry.
        let mut delays = UNARY_RETRY_DELAYS.iter().copied();
        loop {
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
            // TODO: direct push keeps Docker's progress stream; no quiet mode is exposed.
            tokio::select! {
                outcome = push => {
                    let status = outcome.map_err(|error| PushError::Docker {
                        action: "push",
                        diagnostic: error.to_string(),
                    })?;
                    if status.success() {
                        return Ok(());
                    }
                    let Some(delay) = delays.next() else {
                        return Err(PushError::Docker {
                            action: "push",
                            diagnostic: format!("exited with {status}"),
                        });
                    };
                    tokio::time::sleep(delay).await;
                },
                outcome = self.proxy.serve(client.clone(), remote.clone()) => return outcome,
            }
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
        MachineId, MachineName, MachineObservation, MembershipObservation, RpcErrorCode,
        WireGuardPublicKey,
    };
    use serde_json::Value;

    fn machine(seed: u8) -> MachineObservation {
        MachineObservation::new(
            Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: MachineId::parse(format!("{seed:032x}")).unwrap(),
                name: MachineName::parse(format!("machine-{seed}")).unwrap(),
                subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
                public_key: WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            MembershipObservation::Up,
        )
    }

    #[test]
    fn cancellation_preserves_completed_deliveries() {
        let completed = machine(1).machine;
        let cancelled = machine(2).machine;
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        record(&mut result, &completed, Ok(())).unwrap();
        assert!(record(&mut result, &cancelled, Err(PushError::Cancelled)).is_err());
        assert_eq!(result.successes.first().unwrap().machine_id, completed.id);
        assert!(result.failures.is_empty());
        assert_eq!(result.omissions, [cancelled.id]);
    }

    #[test]
    fn registry_port_refusal_names_the_reference_and_retagging_alternative() {
        let image = "localhost:5000/team/api:v1";
        let error = validate_push_reference(image).unwrap_err().to_string();
        assert!(error.contains(image), "{error}");
        assert!(error.contains("retag"), "{error}");
        assert!(error.contains("for example, api:v1"), "{error}");
        assert!(error.contains("without a registry port"), "{error}");
        validate_push_reference("api:v1").unwrap();
    }

    #[test]
    fn invalid_platform_names_the_value_and_accepted_form() {
        for platform in ["linux", "linux//v7", "Linux/amd64", "linux/arm/v7/extra"] {
            let error = validated_platform(platform).unwrap_err().to_string();
            assert!(error.contains(platform), "{error}");
            assert!(error.contains("os/arch[/variant]"), "{error}");
            assert!(error.contains("lowercase"), "{error}");
            assert!(error.contains("linux/amd64"), "{error}");
            assert!(error.contains("linux/arm/v7"), "{error}");
        }
        validated_platform("linux/amd64").unwrap();
        validated_platform("linux/arm/v7").unwrap();
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
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                name: MachineName::parse("all").unwrap(),
                ..machines[0].machine.clone()
            },
            ..machines[0].clone()
        };
        assert_eq!(
            select_targets(&[named_all, machines[1].clone()], &["all".into()])
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
        let error = ingest_error(unsupported);
        assert!(matches!(error, PushError::UnsupportedImageStore));
        let message = error.to_string();
        assert!(message.contains("target Machine"), "{message}");
        assert!(
            message.contains("enable Docker's containerd image store"),
            "{message}"
        );
        assert!(message.contains("on that Machine"), "{message}");
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
        assert_eq!(
            ingest_error(rpc_error(crate::connect::ConnectError::from(
                tonic::Status::unavailable("transport error")
            )))
            .to_string(),
            "Cluster operation failed: image ingest: transport error"
        );
    }
}
