//! Reuse completed images only after observing complete content and placement coverage.
use super::{BuiltService, CapturedBuild, CapturedTarget, platforms::placeable};
use crate::connect::Client;
use ployz_core::{
    DeployIntent, MachineId, MachineObservation, Namespace, PartialResult, RequestedServiceSpec,
    RpcError,
};
use tokio_util::sync::CancellationToken;

impl CapturedBuild {
    /// Remove targets whose previous image is still complete somewhere and
    /// covers every Machine the Service may run on; return those images.
    pub(crate) async fn reuse_images(
        &mut self,
        client: &Client,
        intent: &DeployIntent,
        machines: &[MachineObservation],
        receipts: &[BuiltService],
        cancellation: &CancellationToken,
    ) -> Vec<BuiltService> {
        if receipts.is_empty() {
            return Vec::new();
        }
        let Some(stores) = image_stores(client, machines, cancellation).await else {
            return Vec::new();
        };
        let mut reused = Vec::new();
        let mut remaining = Vec::new();
        for captured in std::mem::take(&mut self.targets) {
            match reusable(&captured, intent, machines, receipts, &stores) {
                Some(image) => reused.push(image),
                None => remaining.push(captured),
            }
        }
        self.targets = remaining;
        reused
    }
}

/// The first of `receipts` for `captured` whose image covers its platforms, runs
/// everywhere it may be placed, and a Machine still holds.
fn reusable(
    captured: &CapturedTarget,
    intent: &DeployIntent,
    machines: &[MachineObservation],
    receipts: &[BuiltService],
    stores: &ImageStores,
) -> Option<BuiltService> {
    let spec = intent
        .target
        .iter()
        .find(|spec| spec.name == captured.name)?;
    receipts
        .iter()
        .filter(|receipt| receipt.name == captured.name)
        .find_map(|receipt| {
            let covers_platforms = captured
                .target
                .platforms
                .iter()
                .all(|platform| receipt.built.platforms.contains(platform));
            if !covers_platforms {
                return None;
            }
            let mut image = receipt.clone();
            image.machine_id = place(
                &receipt.built,
                receipt.machine_id,
                spec,
                &intent.namespace,
                machines,
                stores,
            )?;
            Some(image)
        })
}

/// The Machine that can serve `built` for `spec`: one that runs it everywhere `spec`
/// may be placed and still holds the image. `preferred` is the Machine of the
/// receipt, kept while it qualifies.
pub(crate) fn place(
    built: &ployz_build::BuiltImage,
    preferred: MachineId,
    spec: &RequestedServiceSpec,
    namespace: &Namespace,
    machines: &[MachineObservation],
    stores: &ImageStores,
) -> Option<MachineId> {
    runs_everywhere(built, spec, namespace, machines)
        .then(|| holder(stores, built, preferred))
        .flatten()
}

/// The reads were cancelled before every Machine's image store was listed.
pub(crate) struct Cancelled;

/// [`place`] for one image, listing the Machines' image stores only once `built` runs
/// everywhere `spec` may be placed.
pub(crate) async fn place_listing(
    client: &Client,
    built: &ployz_build::BuiltImage,
    preferred: MachineId,
    spec: &RequestedServiceSpec,
    namespace: &Namespace,
    machines: &[MachineObservation],
    cancellation: &CancellationToken,
) -> Result<Option<MachineId>, Cancelled> {
    if !runs_everywhere(built, spec, namespace, machines) {
        return Ok(None);
    }
    let stores = image_stores(client, machines, cancellation)
        .await
        .ok_or(Cancelled)?;
    Ok(place(built, preferred, spec, namespace, machines, &stores))
}

/// Every Machine's image store, or the Machines that couldn't be asked.
pub(crate) type ImageStores = PartialResult<crate::cluster::MachineImagesObservation, RpcError>;

/// Whether `built` has a platform for every Machine `spec` may be placed on.
fn runs_everywhere(
    built: &ployz_build::BuiltImage,
    spec: &RequestedServiceSpec,
    namespace: &Namespace,
    machines: &[MachineObservation],
) -> bool {
    placeable(spec, namespace, machines).all(|machine| {
        built.platforms.iter().any(|platform| {
            crate::image::platform_compatible(platform, &machine.machine.runtime.architecture)
        })
    })
}

/// Every reachable Machine's image store; `None` once cancelled.
pub(crate) async fn image_stores(
    client: &Client,
    machines: &[MachineObservation],
    cancellation: &CancellationToken,
) -> Option<ImageStores> {
    let visible = machines
        .iter()
        .filter(|machine| machine.invites_rpc())
        .map(|machine| machine.machine.clone())
        .collect::<Vec<_>>();
    tokio::select! {
        biased;
        () = cancellation.cancelled() => None,
        stores = client.list_images(None, &visible) => Some(stores),
    }
}

/// A Machine holding `built` for every one of its platforms. Keep the receipt's
/// Machine (`preferred`) while it still holds the image, so the next receipt keeps
/// naming the Machine with the warm build cache.
fn holder(
    stores: &ImageStores,
    built: &ployz_build::BuiltImage,
    preferred: MachineId,
) -> Option<MachineId> {
    stores
        .successes
        .iter()
        .filter(|source| {
            built.platforms.iter().all(|platform| {
                crate::image::holds_platform(&source.value.images, &built.reference, platform)
            })
        })
        .min_by_key(|source| source.machine_id != preferred)
        .map(|source| source.machine_id)
}
