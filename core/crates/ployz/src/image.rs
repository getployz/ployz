use std::future::Future;

use ployz_core::{
    EnsureImageIngestRequest, FanoutSelector, ImageIngestDestination, ImageIngestReason, Machine,
    MachineFailure, MachineId, MachineImages, MachineSuccess, MachineTarget, PartialResult,
    PeerImagePull, PullImageFromMachineRequest, PullPolicy, RpcError, op,
    resolve_machine_selectors,
};
use thiserror::Error;

use crate::connect::{Client, rpc_error};

mod built;
mod cleanup;
pub use built::push_from_machine;
pub(crate) use built::{
    available_variant, holds_platform, platform_compatible, push_from_machine_using_machines,
};
pub use cleanup::prune_images;
pub(crate) use cleanup::prune_targets;

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
    #[error(
        "Docker on the target Machine is not using the required containerd image store; enable Docker's containerd image store on that Machine, then retry"
    )]
    UnsupportedImageStore,
}

impl PushError {
    pub(crate) fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled)
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
            .filter(|observation| !observation.membership.invites_rpc())
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
