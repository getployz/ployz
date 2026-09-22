//! Reuse completed images only after observing complete content and placement coverage.
use super::{BuildLocation, BuiltService, CapturedBuild, ComposeError};
use crate::{compose::CapturedCompose, connect::Client};
use ployz_core::{MachineObservation, ServicePlacementEligibility};
use std::collections::BTreeSet;
use tokio_util::sync::CancellationToken;

impl CapturedBuild {
    pub(crate) async fn reuse_images(
        &mut self,
        client: &Client,
        candidate: &CapturedCompose,
        machines: &[MachineObservation],
        receipts: &[BuiltService],
        cancellation: &CancellationToken,
    ) -> Result<Vec<BuiltService>, ComposeError> {
        if receipts.is_empty() {
            return Ok(Vec::new());
        }
        let visible = machines
            .iter()
            .filter(|machine| machine.membership.invites_rpc())
            .map(|machine| machine.machine.clone())
            .collect::<Vec<_>>();
        let stores = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(Vec::new()),
            stores = client.list_images(None, &visible) => stores,
        };
        let mut reused = Vec::new();
        for target in self.targets()? {
            let Some(receipt) = receipts.iter().find(|receipt| receipt.name == target.name) else {
                continue;
            };
            let Some(spec) = candidate
                .intent()
                .target
                .iter()
                .find(|spec| spec.name.as_str() == target.name)
            else {
                continue;
            };
            if !target
                .platforms
                .iter()
                .all(|platform| receipt.built.platforms.contains(platform))
                || machines
                    .iter()
                    .filter(|machine| machine.membership != ployz_core::MembershipObservation::Down)
                    .filter(|machine| {
                        !matches!(
                            spec.placement_eligibility_in_project(
                                &candidate.intent().project_name,
                                &machine.machine,
                                machine.storage.as_ref()
                            ),
                            ServicePlacementEligibility::Ineligible(_)
                        )
                    })
                    .any(|machine| {
                        !receipt.built.platforms.iter().any(|platform| {
                            crate::image::platform_compatible(
                                platform,
                                &machine.machine.runtime.architecture,
                            )
                        })
                    })
            {
                continue;
            }
            let Some(source) = stores.successes.iter().find(|source| {
                receipt.built.platforms.iter().all(|platform| {
                    crate::image::holds_platform(
                        &source.value.images,
                        &receipt.built.reference,
                        platform,
                    )
                })
            }) else {
                continue;
            };
            let mut image = receipt.clone();
            image.location = BuildLocation::Machine(source.machine_id);
            reused.push(image);
        }
        let names: BTreeSet<_> = reused.iter().map(|image| image.name.as_str()).collect();
        self.plan
            .retain(|service| !names.contains(service.name.as_str()));
        self.targets
            .retain(|target| !names.contains(target.name.as_str()));
        self.railpack
            .retain(|recipe| !names.contains(recipe.name.as_str()));
        Ok(reused)
    }
}
