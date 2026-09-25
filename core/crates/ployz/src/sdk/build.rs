//! One Image Build: the per-Service call Cloud fans out, over `prepare`'s build path.
use std::time::Duration;

use ployz_core::RpcError;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::preparation::{self, BuildReceipt, OutsideBuildInput, PreparationInput};
use super::prepare::{PreparationError, build_images};
use super::running::Reporter;
use crate::connect::Client;

/// How one Image Build call ended, short of failure.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuildOutcome {
    /// No Build Machine admitted the build within its start limit; it was withdrawn.
    Queued,
    /// The build finished; its receipt identifies the image.
    Built { receipt: BuildReceipt },
}

/// Build the one Service; withdraw it as `Queued` if no Build Machine admits
/// it within `start_within`. An admitted build always runs to its end.
pub(super) async fn run(
    mut client: Client,
    input: PreparationInput,
    start_within: Option<Duration>,
    token: CancellationToken,
    reporter: Reporter,
) -> Result<BuildOutcome, RpcError> {
    let captured = super::capture(input).await?;
    if captured.build.targets().count() != 1 || captured.fingerprints.len() != 1 {
        return Err(super::invalid_argument(
            "build input must hold exactly one Git Service with its source commit".into(),
        ));
    }
    let admit_by = start_within.map(|limit| tokio::time::Instant::now() + limit);
    let result = build_images(
        &mut client,
        &captured.intent,
        captured.build,
        &captured.reusable,
        captured.preference,
        admit_by,
        &token,
        &|progress| reporter.report(progress),
    )
    .await;
    match result {
        Ok(builds) => preparation::receipts(&captured.fingerprints, &builds)
            .into_values()
            .next()
            .map(|receipt| BuildOutcome::Built { receipt })
            .ok_or_else(|| super::invalid_argument("Build produced no receipt".into())),
        Err(PreparationError::Build(crate::build::Error::Withdrawn)) => Ok(BuildOutcome::Queued),
        Err(error) => Err(super::preparation_error(error, token.is_cancelled())),
    }
}

/// What a Builder outside the Cluster does for the one Git Service in a deployment.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutsideBuild {
    /// Nothing: the receipt is for this commit, runs on every Machine the Service may
    /// be placed on, and `machine_name` still holds its image. The receipt names it.
    Reuse {
        receipt: BuildReceipt,
        machine_name: String,
    },
    /// Build these platforms: the Build Platform Requirement, what the Service's
    /// possible placements run.
    Build { platforms: Vec<String> },
}

/// What [`run`] would do for the one Service in `input`, without a checkout and
/// never building: reuse the receipt's image, or build the platforms its
/// placements run.
pub(super) async fn outside(
    mut client: Client,
    input: OutsideBuildInput,
    token: CancellationToken,
) -> Result<OutsideBuild, RpcError> {
    let intent = preparation::frozen_intent(input.deployment.clone())?;
    let machines = super::prepare::observe_machines(&mut client, &intent, &token)
        .await
        .map_err(|error| super::preparation_error(error, token.is_cancelled()))?;
    if let Some(receipt) = input.receipt.clone()
        && let Some(reused) = reuse(&client, &intent, input, receipt, &machines, &token).await?
    {
        return Ok(reused);
    }
    let platforms = crate::build::placement_platforms(&intent, &machines)
        .map_err(|error| super::invalid_argument(error.to_string()))?;
    Ok(OutsideBuild::Build {
        platforms: platforms.into_iter().collect(),
    })
}

async fn reuse(
    client: &Client,
    intent: &ployz_core::DeployIntent,
    input: OutsideBuildInput,
    receipt: BuildReceipt,
    machines: &[ployz_core::MachineObservation],
    token: &CancellationToken,
) -> Result<Option<OutsideBuild>, RpcError> {
    let [spec] = intent.target.as_slice() else {
        return Err(super::invalid_argument(
            "the deployment must hold exactly one Service".into(),
        ));
    };
    // Fingerprints are keyed by Service, as `capture` computes them.
    let fingerprints = preparation::expected_fingerprints(
        input.deployment,
        std::collections::BTreeMap::from([(spec.name.clone(), input.commit)]),
    )?;
    if fingerprints.get(&spec.name) != Some(&receipt.fingerprint)
        || receipt.image.platforms.is_empty()
        || !crate::build::runs_everywhere(&receipt.image, spec, &intent.project_name, machines)
    {
        return Ok(None);
    }
    let Some(stores) = crate::build::image_stores(client, machines, token).await else {
        return Err(super::preparation_error(PreparationError::Cancelled, true));
    };
    let Some(machine_id) = crate::build::holder(&stores, &receipt.image, receipt.machine_id) else {
        return Ok(None);
    };
    let machine_name = machines
        .iter()
        .find(|observed| observed.machine.id == machine_id)
        .map_or_else(
            || machine_id.to_string(),
            |observed| observed.machine.name.to_string(),
        );
    Ok(Some(OutsideBuild::Reuse {
        receipt: BuildReceipt {
            machine_id,
            ..receipt
        },
        machine_name,
    }))
}
