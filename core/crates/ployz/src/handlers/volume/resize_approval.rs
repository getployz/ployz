//! A Device's approval of a resize. When a Server of the Cluster enforces Device approval,
//! `ployz volume set --size` on a deployed Volume signs the size it asks Cloud for, whether a
//! copy may shrink, and every Server that reports a copy. Each of those Servers then sets that
//! size and no other, and lowers its bound only when the approval says it may.

use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedVolumeChange, Signed, Signer, SignerKey, Timestamp, VolumeApproval, VolumeWorkflow,
    validity::MAX_LIFETIME_MILLIS,
};
use ployz_core::{MachineId, OperationId, ProvisionedVolumeMaximumBytes, RpcErrorCode};
use ployz_store::{EnvironmentSummary, VolumeId, VolumeName};
use serde_json::json;

use super::super::deploy::approve::{Probe, probe};
use super::super::{Error, leaf_matches, store};
use super::restore::{Seen, copies, docker_volume};
use crate::ui::Hint;

/// A signed Resize approval and the change it approved.
pub(super) struct ResizeApproval {
    pub(super) signed: Signed<VolumeApproval>,
    pub(super) change: ApprovedVolumeChange,
}

/// Sign the resize of `volume` to `maximum_bytes` with this CLI's Device key, when it holds
/// one and a Server of the Cluster enforces Device approval, after the person reads the change
/// and says yes. A resize to `recorded`, the size the Store holds, or below it may shrink a
/// copy, as Cloud's run asks. None when there is nothing to sign: no Device key, no Server
/// enforcing, or no Server reporting a copy.
///
/// # Errors
/// Refuses when a Server enforces but none reports the Cluster's key, and before signing when
/// the person declines or no terminal can ask and `--yes` wasn't given; returns the Store's,
/// the Servers' or the key's error.
pub(super) fn sign(
    root: &ArgMatches,
    store: &store::Store<'_>,
    (volume, volume_id, environment): (&VolumeName, &VolumeId, &EnvironmentSummary),
    maximum_bytes: ProvisionedVolumeMaximumBytes,
    recorded: Option<ProvisionedVolumeMaximumBytes>,
) -> Result<Option<ResizeApproval>, Error> {
    let matches = leaf_matches(root);
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    let enforcing = match probe(matches) {
        Probe::Off => return Ok(None),
        Probe::Unknown => {
            crate::ui::warn(
                "Couldn't ask your Servers whether they enforce device approval, so this \
                 device didn't approve the resize. Servers that enforce will refuse it.",
            );
            return Ok(None);
        }
        Probe::NoClusterKey => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                "Your Servers enforce device approval, but none reported the Cluster's key, \
                 so this device can't approve the resize. Run ployz status, then try again.",
            ));
        }
        Probe::Enforcing(enforcing) => enforcing,
    };
    let (namespace, docker) = docker_volume(store, volume, volume_id, environment)?;
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    let seen = super::super::runtime()?.block_on(async {
        let client = super::super::server::connect(matches, context).await?;
        copies(&client, &docker).await
    })?;
    let machines = holders(&seen);
    if machines.is_empty() {
        return Ok(None);
    }
    let signer = Signer::from_seed(&secret);
    let issued_at = Timestamp::now();
    let body = VolumeApproval {
        cluster: enforcing.cluster,
        operation: OperationId::random(),
        namespace,
        volume: docker,
        workflow: VolumeWorkflow::Resize {
            machines,
            maximum_bytes,
            shrink: recorded.is_some_and(|recorded| maximum_bytes <= recorded),
        },
        issued_at,
        expires_at: issued_at.plus(MAX_LIFETIME_MILLIS),
        signer: SignerKey::Device(signer.public().into()),
    };
    let change = ApprovedVolumeChange::of(&body, volume.as_str(), |id| {
        seen.iter()
            .find(|seen| seen.id == *id)
            .map_or_else(|| id.to_string(), |seen| seen.name.clone())
    })
    .expect("a Resize approval names a change");
    crate::ui::note(format_args!("{}", change.line()));
    let yes = matches
        .try_get_one::<bool>("yes")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    consent(volume, yes, |question, refusal, declined| {
        crate::ui::confirm(question, || refusal, declined)
    })?;
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    Ok(Some(ResizeApproval { signed, change }))
}

/// Every Server that reports a copy of the Volume, Writer or slot, sorted and distinct as the
/// approval holds them. Cloud's run sets each.
fn holders(seen: &[Seen]) -> Vec<MachineId> {
    let mut machines: Vec<MachineId> = seen
        .iter()
        .filter(|seen| matches!(seen.copy, Ok(Some(_))))
        .map(|seen| seen.id)
        .collect();
    machines.sort_unstable();
    machines.dedup();
    machines
}

/// What a person says to signing the resize, asked after its line shows. `--yes` answers for a
/// run nobody watches; without it, a run that can't ask refuses before signing.
fn consent(
    volume: &VolumeName,
    yes: bool,
    confirm: impl FnOnce(&str, Error, &'static str) -> Result<(), Error>,
) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    let refusal = Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        "Your servers enforce device approval, so signing this resize needs your yes, and \
         there's no terminal to ask in. Read the line above, then add --yes to sign it. \
         Nothing changed.",
        json!({ "volume": volume }),
    )
    .hint(Hint::Retry(super::super::typed_with(&["--yes"])));
    confirm(
        &format!("Approve resizing {volume}?"),
        refusal,
        "Cancelled. Nothing signed or resized.",
    )
}

#[cfg(test)]
#[path = "resize_approval_tests.rs"]
mod tests;
