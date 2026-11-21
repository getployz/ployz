//! Where each Managed Volume's first copy goes. The first Deployment to record a Deploy
//! Preview that makes one decides, in the transaction that records it, before the runner
//! sends anything. While no Server holds a copy, every later plan starts the Volume only
//! there, so a late Prepare or Create from one Deploy and another Deploy never make two.
//! A row outlives the Deployments that wrote it, and goes only with its Volume, whose
//! next add has a new ID, or with the Cluster. A plan whose view of the Cluster omits the
//! Server waits for it rather than starting the Volume elsewhere.

use std::collections::BTreeMap;

use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::{DeployPreview, DockerVolumeName, MachineId, RpcError};

use crate::error;
use crate::id::EnvironmentId;
use crate::storage::Tx;

/// The Server each Volume of `environment` starts on, by Volume ID.
pub(crate) fn first_copies(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeMap<String, MachineId>, RpcError> {
    tx.query(
        "SELECT volume_id, machine_id FROM config_volume_first_copy WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?
    .iter()
    .map(|row| {
        let machine = row
            .text(1)?
            .parse()
            .map_err(|_| error::corrupt("Volume first copy"))?;
        Ok((row.text(0)?.to_owned(), machine))
    })
    .collect()
}

/// Record the Server of each first copy `preview` makes. One the row names already
/// stands. Writes nothing and returns the Volume's name when another Deployment placed
/// it elsewhere first.
pub(crate) fn place(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
    preview: &DeployPreview,
) -> Result<Option<String>, RpcError> {
    let placed = first_copies(tx, environment)?;
    let mut writes = Vec::new();
    for create in &preview.volumes_to_create {
        if create.first_copy.is_none() {
            continue;
        }
        let Some(volume) = intent.volumes.iter().find(|volume| {
            DockerVolumeName::for_resource(&volume.resource_id)
                .is_ok_and(|name| preview.namespace.volume_name(&name) == create.name)
        }) else {
            continue;
        };
        match placed.get(&volume.resource_id) {
            Some(machine) if *machine == create.machine_id => {}
            Some(_) => return Ok(Some(volume.name.clone())),
            None => writes.push((&volume.resource_id, create.machine_id)),
        }
    }
    for (volume, machine) in writes {
        tx.execute(
            "INSERT INTO config_volume_first_copy (environment_id, volume_id, machine_id) \
             VALUES (?1, ?2, ?3)",
            &[
                environment.as_str().into(),
                volume.as_str().into(),
                machine.to_string().as_str().into(),
            ],
        )?;
    }
    Ok(None)
}
