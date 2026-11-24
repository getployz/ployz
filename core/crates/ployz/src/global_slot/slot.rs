//! A Service's copies on one Machine.

use ployz_core::{MachineId, QualifiedService, RpcError};

use crate::deploy::CopyLifecycle;
use crate::deploy::cleanup::Copy;

/// `machine_id`'s copies of `service`, as every Machine lists it.
pub(crate) async fn copies_on<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
) -> Result<Vec<Copy>, RpcError> {
    Ok(client
        .service_containers(service)
        .await?
        .iter()
        .filter(|observation| observation.machine_id == *machine_id)
        .map(Copy::observed)
        .collect())
}
