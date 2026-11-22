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
    let summary = format!(
        "Approving Server {} on {} with endpoints {} (join code {}).",
        body.machine.name,
        body.machine.subnet,
        if endpoints.is_empty() {
            "none"
        } else {
            &endpoints
        },
        body.code()
    );
    let devices = body
        .devices
        .iter()
        .map(|device| device.code().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mut summary = format!("{summary} The Server will trust device {devices}.");
    if !body.passkeys.is_empty() {
        let passkeys = body
            .passkeys
            .iter()
            .map(|passkey| format!("{} for {}", passkey.code(), passkey.account))
            .collect::<Vec<_>>()
            .join(", ");
        summary = format!(
            "{summary} The Server will also trust passkey {passkeys}. A passkey approves workflows, automatic deploy rules, volume changes, Release Channels, and Server joins and key changes in a bundle."
        );
    }
    if !body.grants.is_empty() {
        let grants = body
            .grants
            .iter()
            .map(grant_summary)
            .collect::<Vec<_>>()
            .join("; ");
        summary = format!(
            "{summary} The Server will also trust {grants}. These keys sign service deploys only, in the places named."
        );
    }
    summary
}

/// One CI or agent key as a person reads it before they sign: its kind, code, places and
/// the volumes it may mount.
fn grant_summary(grant: &ployz_core::authority::Grant) -> String {
    let places = match &grant.scope.places {
        ployz_core::authority::KeyPlaces::Cluster => "every environment".to_owned(),
        ployz_core::authority::KeyPlaces::Only(places) => places
            .iter()
            .map(|place| match &place.service {
                Some(service) => format!(
                    "service {} in {} (environment {})",
                    service.name, place.namespace, place.environment
                ),
                None => format!("{} (environment {})", place.namespace, place.environment),
            })
            .collect::<Vec<_>>()
            .join(", "),
    };
    let kind = ployz_core::authority::KeyKind::from(grant.kind);
    let volumes = if grant.volumes.is_empty() {
        "no volumes".to_owned()
    } else {
        let names = grant
            .volumes
            .iter()
            .map(|volume| volume.name.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("volumes {names}")
    };
    format!(
        "{kind} key {} for {places}, with {volumes}",
        grant.key.code()
    )
}

/// Where the join this CLI signs came from, which decides what the signed body may name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JoinEntry {
    /// Through Cloud, which could name a Cluster or a Device of its own.
    Relayed,
    /// Over a route the person chose, so the body is the entry's own word. `pins_its_devices`
    /// when the joiner's pin also holds the Devices the entry trusts, as `server add` sends.
    Chosen { pins_its_devices: bool },
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
    let left_out = narrow_pinned_join(&mut body, signing.device_key(), entry, |cluster| {
        crate::sealing_pin::Pins::beside(config).knows(cluster)
    })?;
    show_join(&body);
    if !left_out.is_empty() {
        ui::note(left_out_summary(&left_out));
    }
    let approval = sign_join(&signing, &mut body)?;
    client
        .call::<op::ApproveJoin>(ployz_core::ApproveJoinRequest { approval }, Call::keyed())
        .await?;
    Ok(true)
}

/// Narrow a pinned join's `body` to what its joiner installs, answering the Devices left out.
/// The joiner trusts no passkey or grant, and only the pinned Devices the body names. A
/// relayed body is signed only for a Cluster this computer pinned.
fn narrow_pinned_join(
    body: &mut ployz_core::authority::JoinApproval,
    own: DeviceKey,
    entry: JoinEntry,
    pinned: impl FnOnce(ployz_core::SealingPublicKey) -> Result<bool, crate::sealing_pin::PinError>,
) -> Result<Vec<DeviceKey>, Error> {
    body.passkeys.clear();
    body.grants.clear();
    if entry == JoinEntry::Relayed && !pinned(body.cluster)? {
        return Err(Error::conflict(format!(
            "Server {} would join a Cluster this computer hasn't pinned, and the join was relayed through Ployz Cloud, which can't vouch for it. Pin your Cluster's key from one of its Servers, then add the Server again. Nothing was approved.",
            body.machine.name
        ))
        .hint(Hint::Next(crate::sealing_pin::PIN_COMMAND.to_owned())));
    }
    if entry
        == (JoinEntry::Chosen {
            pins_its_devices: true,
        })
    {
        return Ok(Vec::new());
    }
    if !body.devices.contains(&own) {
        return Err(Error::conflict(format!(
            "The Server admitting {} doesn't list this computer's device {}, so this computer can't approve the join. Nothing was approved.",
            body.machine.name,
            own.code()
        )));
    }
    let left_out = body
        .devices
        .iter()
        .copied()
        .filter(|device| *device != own)
        .collect();
    body.devices = vec![own];
    Ok(left_out)
}

fn left_out_summary(left_out: &[DeviceKey]) -> String {
    let codes = left_out
        .iter()
        .map(|device| device.code().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Not trusting device {codes} on the new Server: only this computer's device is pinned for the join. Approve them there afterwards with ployz device approve <code>."
    )
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
