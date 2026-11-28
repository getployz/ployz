//! `ployz key add pk1_…`: trust the key from an install command on this Server, the
//! recovery when no device or passkey this Server trusts can approve another.

use std::path::Path;

use ployz_core::first_key::LineKey;
use ployz_core::{
    DeviceSet, InspectRequest, MachineLayout, MachineTarget, RpcError, RpcErrorCode, SetKeyRequest,
    op,
};
use serde::Serialize;

use super::{Error, runtime};
use crate::cluster::{Call, fan_out_entry_last};
use crate::first_key_line;

/// `key add` with a `line`: re-arm the First Key before this Server enrolls, or trust the
/// key over the local socket after.
pub(super) fn add(line: &str) -> Result<(), Error> {
    let line = first_key_line::parse(line)?;
    let layout = MachineLayout::production();
    if !first_key_line::enrolled(layout.data_dir())? {
        if !crate::provisioning::process_is_root() {
            return Err(Error::coded(
                RpcErrorCode::Unauthenticated,
                "Run it as root: sudo ployz key add …",
            ));
        }
        first_key_line::rearm(layout.data_dir(), &line)?;
        return crate::ui::finish(
            &Armed {
                waiting: line.hint(),
            },
            || {
                crate::ui::stream(first_key_line::will_trust(&line));
            },
        );
    }
    runtime()?.block_on(add_here(&layout.socket(), line))
}

/// A key this Server, not yet in a Cluster, will trust to join or found one.
#[derive(Serialize)]
struct Armed {
    waiting: ployz_core::first_key::FirstKeyHint,
}

#[derive(Serialize)]
struct Added {
    #[serde(flatten)]
    set: DeviceSet,
    /// Members that don't trust it yet, or didn't say.
    still_without: Vec<String>,
}

/// Trust `line` on the Server behind the local socket, then name the Members still
/// without it, so none is left out silently.
async fn add_here(socket: &Path, line: LineKey) -> Result<(), Error> {
    let mut client = crate::connect::connect(
        Path::new(&crate::context::default_config_path()),
        Some(&format!("unix://{}", socket.display())),
        None,
    )
    .await?;
    let set = client
        .call::<op::SetKey>(SetKeyRequest { key: line.clone() }, Call::keyed())
        .await?;
    let here = client.entry_machine();
    let others: Vec<_> = client
        .machines()
        .await?
        .into_iter()
        .filter(|observed| Some(observed.machine.id) != here)
        .collect();
    let code = line.code();
    let checked = fan_out_entry_last(
        others
            .iter()
            .filter(|observed| observed.invites_rpc())
            .map(|observed| observed.machine.id),
        None,
        |machine_id| {
            let client = client.clone();
            async move {
                let details = client
                    .call_shared::<op::Inspect>(
                        InspectRequest::default(),
                        Call::once().to(&MachineTarget::from(&machine_id)),
                    )
                    .await
                    .map_err(crate::connect::rpc_error)?;
                Ok::<_, RpcError>(Some(
                    details.devices.iter().any(|key| key.code() == code)
                        || details
                            .passkeys
                            .iter()
                            .any(|trusted| trusted.passkey.code() == code),
                ))
            }
        },
    )
    .await;
    let still_without: Vec<String> = others
        .iter()
        .filter(|observed| {
            !checked
                .successes
                .iter()
                .any(|success| success.machine_id == observed.machine.id && success.value)
        })
        .map(|observed| observed.machine.name.to_string())
        .collect();
    let added = Added { set, still_without };
    crate::ui::finish(&added, || {
        crate::ui::stream(match added.set {
            DeviceSet::AlreadyApproved { .. } => {
                format!("This Server already trusts {}.", line.describe())
            }
            DeviceSet::Approved { .. }
            | DeviceSet::Removed { .. }
            | DeviceSet::NotPresent { .. } => {
                format!("This Server now trusts {}.", line.describe())
            }
        });
        if !added.still_without.is_empty() {
            crate::ui::note(format!(
                "It changes this Server only. Run this on each Server that should trust it, starting with {}:\n  sudo ployz key add {line}",
                added.still_without.join(", ")
            ));
        }
    })
}
