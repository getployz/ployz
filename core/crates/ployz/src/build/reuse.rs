//! Reuse completed images only after observing complete content and placement coverage.
use super::{BuiltService, CapturedBuild, CapturedTarget, platforms::placeable};
use crate::connect::Client;
use ployz_core::{DeployIntent, MachineObservation, PartialResult, RpcError};
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
        let visible = machines
            .iter()
            .filter(|machine| machine.membership.invites_rpc())
            .map(|machine| machine.machine.clone())
            .collect::<Vec<_>>();
        let stores = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Vec::new(),
            stores = client.list_images(None, &visible) => stores,
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
    let runs_everywhere = placeable(spec, &intent.project_name, machines).all(|machine| {
        receipt.built.platforms.iter().any(|platform| {
            crate::image::platform_compatible(platform, &machine.machine.runtime.architecture)
        })
    });
    if !covers_platforms || !runs_everywhere {
        return None;
    }
    // Keep the receipt's Machine while it still holds the image, so the next
    // receipt keeps naming the Machine with the warm build cache.
    let source = stores
        .successes
        .iter()
        .filter(|source| {
            receipt.built.platforms.iter().all(|platform| {
                crate::image::holds_platform(
                    &source.value.images,
                    &receipt.built.reference,
                    platform,
                )
            })
        })
        .min_by_key(|source| source.machine_id != receipt.machine_id)?;
    let mut image = receipt.clone();
    image.machine_id = source.machine_id;
    Some(image)
}
