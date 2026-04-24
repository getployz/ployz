pub mod session;

mod execute;
mod managed_domains;
mod plan;
mod probe;

#[cfg(test)]
mod tests;

use crate::deploy::session::DeploySessionFactory;
use crate::error::Result;
use crate::model::{DeployApplyResult, DeployPreview, MachineId};
use plan::resolve_plan;
use ployz_store_api::StoreDriver;
use ployz_types::spec::DeployManifest;
use probe::{probe_participants, warnings_from_reachability};

pub async fn preview(
    store: &StoreDriver,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployPreview> {
    let plan = resolve_plan(store, local_machine_id, manifest).await?;
    let reachability = probe_participants(plan.participants(), plan.machine_map()).await;
    let mut warnings = warnings_from_reachability(&reachability);
    warnings.extend(managed_domains::warnings_for_plan(store, &plan).await?);
    Ok(plan.to_preview(warnings))
}

pub async fn apply(
    store: &StoreDriver,
    session_factory: &dyn DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployApplyResult> {
    execute::apply(store, session_factory, local_machine_id, manifest).await
}
