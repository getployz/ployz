//! Reuse completed images only after observing complete content and placement coverage.
use super::{BuiltService, CapturedBuild, CapturedTarget, platforms::placeable};
use crate::connect::Client;
use ployz_core::{
    DeployIntent, MachineId, MachineObservation, PartialResult, ProjectName, RequestedServiceSpec,
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

fn reusable(
    captured: &CapturedTarget,
    intent: &DeployIntent,
    machines: &[MachineObservation],
    receipts: &[BuiltService],
    stores: &PartialResult<crate::cluster::MachineImagesObservation, RpcError>,
) -> Option<BuiltService> {
    let receipt = receipts
        .iter()
        .find(|receipt| receipt.name == captured.name)?;
    let spec = intent
        .target
        .iter()
        .find(|spec| spec.name == captured.name)?;
    let covers_platforms = captured
        .target
        .platforms
        .iter()
        .all(|platform| receipt.built.platforms.contains(platform));
    if !covers_platforms || !runs_everywhere(&receipt.built, spec, &intent.project_name, machines) {
        return None;
    }
    let mut image = receipt.clone();
    image.machine_id = holder(stores, &receipt.built, receipt.machine_id)?;
    Some(image)
}

/// Whether `built` has a platform for every Machine `spec` may be placed on.
pub(crate) fn runs_everywhere(
    built: &ployz_build::BuiltImage,
    spec: &RequestedServiceSpec,
    project: &ProjectName,
    machines: &[MachineObservation],
) -> bool {
    placeable(spec, project, machines).all(|machine| {
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
) -> Option<PartialResult<crate::cluster::MachineImagesObservation, RpcError>> {
    let visible = machines
        .iter()
        .filter(|machine| machine.membership.invites_rpc())
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
pub(crate) fn holder(
    stores: &PartialResult<crate::cluster::MachineImagesObservation, RpcError>,
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
