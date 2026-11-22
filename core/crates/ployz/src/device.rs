//! This computer's Device: its key file, signing with it, and approving a Server join with it.

use std::path::Path;

use ployz_core::{DeviceKey, Hint, authority::SignerKey, op};

use crate::cluster::Call;
use crate::connect::Client;
use crate::failure::Failure as Error;
use crate::ui;

pub(crate) mod key;
mod signing;
pub(crate) use signing::DeviceSigning;

/// `body` with this Device named as its signer, signed by it.
pub(crate) fn sign_join(
    signing: &DeviceSigning,
    body: &mut ployz_core::authority::JoinApproval,
) -> Result<ployz_core::authority::Signed<ployz_core::authority::JoinApproval>, Error> {
    body.signer = SignerKey::Device(signing.device_key());
    signing.sign(body)
}

pub(crate) fn show_join(body: &ployz_core::authority::JoinApproval) {
    ui::note(join_summary(body));
}

fn join_summary(body: &ployz_core::authority::JoinApproval) -> String {
    let endpoints = body
        .machine
        .advertised_endpoints
        .iter()
        .map(|endpoint| endpoint.0.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Approving Server {} on {} with endpoints {} (join code {}). It will trust the same devices, passkeys and CI or agent keys as the Server that admits it, and none that Server removed.",
        body.machine.name,
        body.machine.subnet,
        if endpoints.is_empty() {
            "none"
        } else {
            &endpoints
        },
        body.code()
    )
}

/// Where the join this CLI signs came from, which decides what the signed body may name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum JoinEntry {
    /// Through Cloud, which could name a Cluster or a Device of its own. Only `pin`, the key
    /// this computer pinned for the Organization it is signed in to, vouches for the Cluster.
    Relayed {
        pin: Option<crate::sealing_pin::Pin>,
    },
    /// Over a route the person chose, so the body is the entry's own word. `pins_its_devices`
    /// when the joiner's pin also holds the Devices the entry trusts, as `server add` sends.
    Chosen { pins_its_devices: bool },
}

impl JoinEntry {
    /// A join relayed through Cloud, vouched for by the pin of the Organization this
    /// computer is signed in to.
    ///
    /// # Errors
    /// Fails when Cloud can't say which Organization the sign-in acts in, or the pins
    /// can't be read.
    pub(crate) async fn relayed(config: &Path) -> Result<Self, Error> {
        Ok(Self::Relayed {
            pin: crate::handlers::device::join_pin(config).await?,
        })
    }
}

/// Sign the join waiting for `expected`, narrowed to what the joiner's pin installs, so the
/// summary shows what the Server will trust.
pub(crate) async fn sign_waiting_join(
    client: &mut Client,
    config: &Path,
    expected: &ployz_core::RegisterRequest,
    entry: JoinEntry,
) -> Result<bool, Error> {
    let Some(signing) = DeviceSigning::load(&key::path(config))? else {
        return Ok(false);
    };
    let pending = client
        .call::<op::PendingJoins>(ployz_core::PendingJoinsRequest {}, Call::read())
        .await?;
    let Some(mut body) = pending
        .joins
        .into_iter()
        .find(|body| body.machine.id == expected.machine_id)
    else {
        return Ok(false);
    };
    if !body.machine.matches_request(expected) {
        return Err(Error::conflict(
            "The waiting join does not match the Server identity, keys, allocation, or policy requested. Nothing was approved.",
        ));
    }
    check_waiting_join(&body, signing.device_key(), &entry)?;
    show_join(&body);
    let approval = sign_join(&signing, &mut body)?;
    client
        .call::<op::ApproveJoin>(ployz_core::ApproveJoinRequest { approval }, Call::keyed())
        .await?;
    Ok(true)
}

/// Refuse to sign a join this computer can't vouch for: a body relayed through Cloud for a
/// Cluster this computer never pinned, or one whose admitting Server doesn't trust this
/// computer's Device. The joiner trusts what its admitting Server seals, not the body's lists.
fn check_waiting_join(
    body: &ployz_core::authority::JoinApproval,
    own: DeviceKey,
    entry: &JoinEntry,
) -> Result<(), Error> {
    if let JoinEntry::Relayed { pin } = entry {
        refuse_unpinned(body, pin.as_ref())?;
    }
    if *entry
        != (JoinEntry::Chosen {
            pins_its_devices: true,
        })
        && !body.devices.contains(&own)
    {
        return Err(Error::conflict(format!(
            "The Server admitting {} doesn't list this computer's device {}, so this computer can't approve the join. Nothing was approved.",
            body.machine.name,
            own.code()
        )));
    }
    Ok(())
}

/// Refuse to sign, with `own`, a join whose Server waits for another key from its install
/// line: it would refuse the approval, and the join would be spent on it.
pub(crate) fn refuse_another_key(
    body: &ployz_core::authority::JoinApproval,
    expected: ployz_core::first_key::FirstKeyHint,
    own: DeviceKey,
) -> Result<(), Error> {
    if expected == (ployz_core::first_key::FirstKeyHint::Device { code: own.code() }) {
        return Ok(());
    }
    let how = match expected {
        ployz_core::first_key::FirstKeyHint::Passkey { .. } => {
            "Approve it with that passkey: choose Approve with passkey on the Servers page in Ployz Cloud."
        }
        ployz_core::first_key::FirstKeyHint::Device { .. } => {
            "Run this command on the computer that holds that device."
        }
    };
    Err(Error::conflict(format!(
        "Server {} trusts only {expected}, not this computer's device {}. {how} Nothing was approved.",
        body.machine.name,
        own.code()
    )))
}

/// Refuse a join relayed through Cloud for a Cluster other than `pin`, the one this
/// computer pinned for the Organization it is signed in to.
pub(crate) fn refuse_unpinned(
    body: &ployz_core::authority::JoinApproval,
    pin: Option<&crate::sealing_pin::Pin>,
) -> Result<(), Error> {
    if pin.is_some_and(|pin| pin.public_key == body.cluster) {
        return Ok(());
    }
    Err(Error::conflict(format!(
        "Server {} would join a Cluster other than the one this computer pinned for the Organization you're signed in to, and the join was relayed through Ployz Cloud, which can't vouch for it. Pin your Cluster's key from one of its Servers, then add the Server again. Nothing was approved.",
        body.machine.name
    ))
    .hint(Hint::Next(crate::sealing_pin::PIN_COMMAND.to_owned())))
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
