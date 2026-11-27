use crate::cluster::Call;
use std::future::Future;

use ployz_core::{
    FanoutSelector, ImageIngestReason, ImagePulled, Machine, MachineFailure, MachineId,
    MachineImages, MachineSuccess, MachineTarget, PULL_IMAGE_CAPABILITY, PartialResult,
    PeerImagePull, PullFreeImageRequest, PullImageRequest, PullPolicy, RpcError, RpcErrorCode,
    SealedLogin, op, resolve_machine_selectors,
};
use thiserror::Error;

use crate::{
    cluster::answered_by,
    coded::Coded,
    connect::{Client, rpc_error},
};

mod built;
mod cleanup;
mod peer;
mod pin;
#[cfg(test)]
pub(crate) use built::push_from_machine;
pub(crate) use built::{
    available_variant, holds_platform, platform_compatible, push_from_machine_using_machines,
};
pub use cleanup::prune_images;
pub(crate) use cleanup::prune_targets;
use peer::PeerSource;
pub use pin::{PinError, pin_public};

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
    #[error("image delivery cancelled")]
    Cancelled,
    #[error(transparent)]
    InvalidSelector(#[from] ployz_core::ValueError),
    #[error(transparent)]
    Selector(#[from] ployz_core::MachineSelectorError),
    #[error(transparent)]
    Cluster(#[from] crate::connect::ConnectError),
    #[error("Could not ingest the image on the Machine.")]
    ImageIngest(#[source] RpcError),
    #[error("Could not pull the image from a peer Machine.")]
    PeerPull(#[source] RpcError),
    #[error(
        "Docker on the target Machine is not using the required containerd image store; enable Docker's containerd image store on that Machine, then retry"
    )]
    UnsupportedImageStore,
}

impl Coded for PushError {
    fn code(&self) -> RpcErrorCode {
        match self {
            PushError::VariantUnavailable { .. } => RpcErrorCode::NotFound,
            PushError::BuildIncomplete { .. } => RpcErrorCode::Conflict,
            PushError::InvalidReference { .. } | PushError::InvalidSelector(_) => {
                RpcErrorCode::InvalidArgument
            }
            PushError::Selector(error) => error.code(),
            PushError::Cluster(error) => error.code(),
            PushError::ImageIngest(error) | PushError::PeerPull(error) => error.code.clone(),
            PushError::UnsupportedImageStore => RpcErrorCode::Unsupported,
            // Ctrl-C or the caller stopped delivery: not a fault; it did not finish and can be retried.
            PushError::Cancelled => RpcErrorCode::Unavailable,
        }
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

pub(crate) struct DeliverySelection {
    pub targets: Vec<Machine>,
    pub omissions: Vec<MachineId>,
}

pub(crate) fn delivery_selection(
    observations: &[ployz_core::MachineObservation],
    selectors: &[String],
) -> Result<DeliverySelection, PushError> {
    let targets = match select_targets(observations, selectors) {
        Ok(targets) => targets,
        Err(PushError::Selector(ployz_core::MachineSelectorError::NoVisibleMachines)) => Vec::new(),
        Err(error) => return Err(error),
    };
    let omissions = if selectors.is_empty() {
        observations
            .iter()
            .filter(|observation| !observation.invites_rpc())
            .map(|observation| observation.machine.id)
            .collect()
    } else {
        Vec::new()
    };
    Ok(DeliverySelection { targets, omissions })
}

fn select_targets(
    observations: &[ployz_core::MachineObservation],
    selectors: &[String],
) -> Result<Vec<Machine>, PushError> {
    let machines = observations
        .iter()
        .filter(|observation| observation.invites_rpc())
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
    // A peer can't serve a tag from a registry with a port, so its registry pull decides.
    let any_server = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
    if ployz_core::image_server_reference(any_server, image).is_none() {
        return Ok(());
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
        .filter(|machine| machine.invites_rpc())
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
    let source = PeerSource::open(&mut listing_client, *peer)
        .await
        .map_err(|error| answered_by(peer, error))?;
    source
        .pull_into(
            &mut listing_client,
            dest,
            PeerImagePull::Reference {
                image: image.to_owned(),
            },
            platform,
        )
        .await
        .map_err(|error| answered_by(dest, error))
}

/// Make `image` present on `dest` as `policy` asks before a Deploy changes any
/// Container. A tag is copied from a peer only where this client may name images
/// ([`Naming::UserTags`]); anywhere else `dest` pulls it as its own creation will read it.
/// Otherwise a peer that holds the image serves it as [`ensure_cluster_image`]
/// does, and `dest` then pulls from its registry as `policy` asks and confirms it holds
/// the image, so `Never` fails here for an absent image. Answers what `dest` reports
/// of the image, such as the ports it exposes.
///
/// # Errors
///
/// Returns when the peer step fails, the registry pull fails, or `dest` does not
/// hold the image afterwards.
pub(crate) async fn pull_cluster_image(
    client: &Client,
    dest: &MachineId,
    image: &str,
    policy: PullPolicy,
    registry_login: Option<SealedLogin>,
) -> Result<ImagePulled, RpcError> {
    let target = MachineTarget::from(dest);
    if !ployz_core::authority::deploy::content_addressed(image)
        && client.image_naming(dest) == Naming::Retention
    {
        // Pulled as the creation will be, so the ports reported are the image that runs.
        let pulled = client
            .clone()
            .call::<op::PullFreeImage>(
                PullFreeImageRequest {
                    image: image.to_owned(),
                    policy,
                    // What a free pull places runs unsigned, so it never carries a login.
                    registry_auth: None,
                    registry_login: None,
                },
                Call::keyed().to(&target),
            )
            .await
            .map_err(|error| answered_by(dest, error).pulling(image, policy));
        match pulled {
            Err(error)
                if error.refusal().is_some_and(|refusal| {
                    refusal.capability == ployz_core::PULL_FREE_IMAGE_CAPABILITY
                }) => {}
            pulled => return pulled,
        }
    } else {
        ensure_cluster_image(client, dest, image, policy).await?;
    }
    client
        .clone()
        .call::<op::PullImage>(
            PullImageRequest {
                image: image.to_owned(),
                policy,
                registry_auth: None,
                registry_login,
            },
            Call::keyed().to(&target),
        )
        .await
        .or_else(|error| pulls_on_create(answered_by(dest, error).pulling(image, policy)))
}

/// A daemon older than [`PULL_IMAGE_CAPABILITY`] pulls when it creates the Container,
/// with the same policy and credentials, so its refusal is not a failure. It reports
/// no exposed ports. A newer one refuses an Operator's tag the same way, and its unsigned
/// creation pulls the image itself.
fn pulls_on_create(error: RpcError) -> Result<ImagePulled, RpcError> {
    match error.refusal() {
        Some(refusal) if refusal.capability == PULL_IMAGE_CAPABILITY => Ok(ImagePulled::default()),
        _ => Err(error),
    }
}

/// Who may name what a copied image is called on its destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Naming {
    /// Only the retention name, which nothing but Image Cleanup reads.
    Retention,
    /// Also the spec's tag, when the destination has none and the source's own tag names
    /// the copied image: a later `never` Deploy of that tag then finds it there.
    UserTags,
}

/// Copy the image a Container runs, by its local image ID, from `source` to `dest`. Never
/// asks a registry: a tag moved since the Deploy must not change what runs after Placement
/// convergence. A digest `image` arrives as it names itself; a tag `image` arrives under
/// its retention name, and with [`Naming::UserTags`] also under `image` when `dest` holds
/// no such tag and `source`'s own `image` tag names this image. An existing tag never
/// moves. Nothing is sent when `dest` already holds the image under every name it needs.
///
/// # Errors
///
/// Returns when either store can't be listed, `source` holds no variant `dest` runs,
/// or opening ingest or the peer pull fails.
pub(crate) async fn copy_running_image(
    client: &Client,
    source: &Machine,
    dest: &Machine,
    image: &str,
    image_id: &str,
    naming: Naming,
) -> Result<(), PushError> {
    let mut client = client.clone();
    let listings = client
        .list_images(None, &[source.clone(), dest.clone()])
        .await;
    if let Some(failure) = listings.failures.into_iter().next() {
        return Err(PushError::Cluster(crate::connect::ConnectError::Remote(
            failure.error,
        )));
    }
    let store = |id: &MachineId| {
        listings
            .successes
            .iter()
            .find(|success| success.machine_id == *id)
            .map(|success| &success.value.images)
    };
    let dest_images = store(&dest.id).expect("every listed target answered");
    let source_images = store(&source.id).expect("every listed target answered");
    let names = if ployz_core::authority::deploy::content_addressed(image) {
        Vec::new()
    } else {
        let pinned = format!("{}@{image_id}", ployz_core::image_repository(image));
        let retention =
            ployz_core::retention_tag(&pinned).ok_or_else(|| PushError::InvalidReference {
                reference: pinned.clone(),
                message: "not pinned to a sha256 digest".to_owned(),
            })?;
        let tag = (naming == Naming::UserTags
            && !built::holds_tag(dest_images, image)
            && built::holds_as(source_images, image, image_id))
        .then(|| image.to_owned());
        std::iter::once(retention).chain(tag).collect()
    };
    let held = available_variant(dest_images, image_id, &dest.runtime.architecture).is_some()
        && names
            .iter()
            .all(|name| built::holds_as(dest_images, name, image_id));
    if held {
        return Ok(());
    }
    if !source_images.containerd_store {
        return Err(PushError::UnsupportedImageStore);
    }
    let Some(platform) = available_variant(source_images, image_id, &dest.runtime.architecture)
    else {
        return Err(PushError::VariantUnavailable {
            image: image_id.to_owned(),
            machine_id: source.id,
            platform: dest.runtime.architecture.clone(),
        });
    };
    let pulls = if names.is_empty() {
        vec![PeerImagePull::Reference {
            image: image.to_owned(),
        }]
    } else {
        let pinned = format!("{}@{image_id}", repository(image));
        let pinned = ployz_core::ImageDigestReference::parse(&pinned).map_err(|error| {
            PushError::InvalidReference {
                reference: pinned.clone(),
                message: error.to_string(),
            }
        })?;
        names
            .into_iter()
            .map(|tag| PeerImagePull::Publish {
                image: pinned.clone(),
                tag,
            })
            .collect()
    };
    let source = PeerSource::open(&mut client, source.id)
        .await
        .map_err(|error| ingest_error(rpc_error(error)))?;
    for pull in pulls {
        source
            .pull_into(&mut client, &dest.id, pull, platform)
            .await
            .map_err(|error| PushError::PeerPull(rpc_error(error)))?;
    }
    Ok(())
}

/// `image` without its tag or registry: the path a digest pull asks the source for.
/// The destination pulls it through its own loopback registry, which cannot nest a
/// registry host like `registry:5000`, and the source serves digests by content alone.
fn repository(image: &str) -> &str {
    let repository = ployz_core::image_repository(image);
    match repository.split_once('/') {
        Some((host, path)) if host.contains(['.', ':']) || host == "localhost" => path,
        _ => repository,
    }
}

fn ingest_error(error: RpcError) -> PushError {
    match error.evidence::<ImageIngestReason>() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::error_chain;
    use ployz_core::{
        MachineId, MachineName, MachineObservation, MembershipObservation, WireGuardPublicKey,
    };

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
                join_approval: None,
                build_concurrency: None,
            },
            MembershipObservation::Up,
        )
    }

    #[test]
    fn target_selection_preserves_the_explicit_contract() {
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
        let broadcast = delivery_selection(&mixed, &[]).unwrap();
        assert_eq!(broadcast.targets.len(), 2);
        assert_eq!(broadcast.omissions.len(), 2);
        let named = delivery_selection(&mixed, &["machine-1".into()]).unwrap();
        assert_eq!(named.targets.len(), 1);
        assert!(named.omissions.is_empty());
    }

    #[test]
    fn repository_drops_the_tag_and_registry() {
        assert_eq!(repository("127.0.0.1:5000/app:latest"), "app");
        assert_eq!(repository("registry:5000/team/app"), "team/app");
        assert_eq!(repository("ghcr.io/team/app:v1"), "team/app");
        assert_eq!(repository("localhost/app"), "app");
        assert_eq!(repository("team/app:v1"), "team/app");
        assert_eq!(repository("alpine"), "alpine");
    }

    #[test]
    fn a_daemon_without_image_pull_is_left_to_pull_on_create() {
        let refusal = |capability: &str| {
            RpcError::unsupported("unimplemented").with_details(
                serde_json::json!({"capability": capability, "target": "a".repeat(32)}),
            )
        };
        assert!(pulls_on_create(refusal(PULL_IMAGE_CAPABILITY)).is_ok());
        assert!(pulls_on_create(refusal("ployz.serving.routes.v1")).is_err());
        let failed = RpcError::unavailable("registry unreachable");
        assert_eq!(pulls_on_create(failed.clone()), Err(failed));
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
            ingest_error(RpcError::unavailable("ingest unavailable")),
            PushError::ImageIngest(_)
        ));
        let error = ingest_error(rpc_error(crate::connect::ConnectError::from(
            tonic::Status::unavailable("transport error"),
        )));
        assert_eq!(
            error.to_string(),
            "Could not ingest the image on the Machine."
        );
        assert_eq!(error_chain::causes(&error), ["transport error"]);
    }
}
