use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use crate::StoreDriver;
use crate::error::{Error, Result};
use crate::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusEvidence, InstanceStatusGoal,
    InstanceStatusRecord, InstanceStatusTransition, MachineId, SlotId,
};
use crate::spec::{Namespace, ServiceSpec, VolumeDeclaration};
use ployz_store_api::InstanceStatusStore;
use ployz_volume_api::VolumeBackend;

use super::local::{
    LocalDeployRuntime, StartCandidate, adopt_instances, build_instance_status_record,
    list_local_instance_status, now_unix_secs,
};
/// Server-side deploy agent. Shared across local and NATS-routed commands.
#[derive(Clone)]
pub struct DeployAgent {
    store: StoreDriver,
    local_machine_id: MachineId,
    overlay_network_name: Option<String>,
    overlay_dns_server: Option<Ipv4Addr>,
    volume_backend: Option<Arc<dyn VolumeBackend>>,
}

/// Runtime context for one deploy participant command. Coordination locks are
/// owned by the deploy coordinator, not by participant command handlers.
pub struct DeployCommandContext {
    namespace: Namespace,
}

impl DeployAgent {
    #[must_use]
    pub fn new(
        store: StoreDriver,
        local_machine_id: MachineId,
        overlay_network_name: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
        volume_backend: Option<Arc<dyn VolumeBackend>>,
    ) -> Self {
        Self {
            store,
            local_machine_id,
            overlay_network_name,
            overlay_dns_server,
            volume_backend,
        }
    }

    /// Adopt orphaned containers and return a snapshot of current instances.
    pub async fn inspect_namespace(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        if let Ok(runtime) = self.new_runtime() {
            adopt_instances(&self.store, &runtime, namespace).await?;
        }
        list_local_instance_status(&self.store, namespace, &self.local_machine_id).await
    }

    #[must_use]
    pub fn command_context(&self, namespace: Namespace) -> DeployCommandContext {
        DeployCommandContext { namespace }
    }

    pub async fn start_candidate(
        &self,
        context: &DeployCommandContext,
        service: &str,
        slot_id: &SlotId,
        instance_id: &InstanceId,
        deploy_id: &DeployId,
        spec_json: &str,
        volumes_json: &str,
    ) -> Result<InstanceStatusRecord> {
        // Idempotent: if instance already exists, return its status.
        if let Some(existing) = self
            .find_local_instance_status(&context.namespace, instance_id)
            .await?
        {
            return Ok(existing);
        }

        let spec: ServiceSpec = serde_json::from_str(spec_json)
            .map_err(|e| Error::operation("start_candidate", format!("decode spec: {e}")))?;
        let volumes: Vec<VolumeDeclaration> = serde_json::from_str(volumes_json)
            .map_err(|e| Error::operation("start_candidate", format!("decode volumes: {e}")))?;
        let volumes = volumes
            .into_iter()
            .map(|volume| (volume.name.clone(), volume))
            .collect::<HashMap<_, _>>();
        if spec.name != service {
            return Err(Error::operation(
                "start_candidate",
                format!(
                    "spec service '{}' did not match request service '{}'",
                    spec.name, service
                ),
            ));
        }
        let revision_hash = spec
            .revision_hash()
            .map_err(|e| Error::operation("start_candidate", e))?;
        let runtime = self.new_runtime()?;
        let instance = runtime
            .start_candidate(StartCandidate {
                namespace: &context.namespace,
                spec: &spec,
                deploy_id,
                instance_id,
                slot_id,
                machine_id: &self.local_machine_id,
                revision_hash: &revision_hash,
                volumes: &volumes,
            })
            .await?;
        runtime.wait_ready(&spec, &instance).await?;
        let status = build_instance_status_record(
            &context.namespace,
            &instance,
            InstancePhase::Ready,
            true,
            DrainState::None,
            None,
        );
        self.store.record_instance_status(&status).await?;
        Ok(status)
    }

    pub async fn drain_instance(
        &self,
        context: &DeployCommandContext,
        instance_id: &InstanceId,
    ) -> Result<()> {
        let Some(mut status) = self
            .find_local_instance_status(&context.namespace, instance_id)
            .await?
        else {
            // Idempotent: already gone is not an error.
            return Ok(());
        };
        // Idempotent: already draining is not an error.
        if status.phase == InstancePhase::Draining {
            return Ok(());
        }
        status
            .apply_status_transition(InstanceStatusTransition {
                goal: InstanceStatusGoal::MarkDraining,
                evidence: InstanceStatusEvidence::DeployCleanup {
                    deploy_id: status.deploy_id.clone(),
                },
                at_unix_secs: now_unix_secs(),
            })
            .map_err(|error| Error::operation("instance_status_transition", error.to_string()))?;
        self.store.record_instance_status(&status).await?;
        Ok(())
    }

    pub async fn remove_instance(
        &self,
        context: &DeployCommandContext,
        instance_id: &InstanceId,
    ) -> Result<()> {
        let Some(status) = self
            .find_local_instance_status(&context.namespace, instance_id)
            .await?
        else {
            // Idempotent: already gone is not an error.
            return Ok(());
        };
        let runtime = self.new_runtime()?;
        runtime
            .remove_instance(&status.instance_id, &context.namespace, &status.service)
            .await?;
        self.store
            .remove_instance_status(&status.instance_id)
            .await?;
        Ok(())
    }

    #[must_use]
    pub fn local_machine_id(&self) -> &MachineId {
        &self.local_machine_id
    }

    fn new_runtime(&self) -> Result<LocalDeployRuntime> {
        LocalDeployRuntime::new(
            self.overlay_network_name.clone(),
            self.overlay_dns_server,
            self.volume_backend.clone(),
        )
    }

    async fn find_local_instance_status(
        &self,
        namespace: &Namespace,
        instance_id: &InstanceId,
    ) -> Result<Option<InstanceStatusRecord>> {
        Ok(
            list_local_instance_status(&self.store, namespace, &self.local_machine_id)
                .await?
                .into_iter()
                .find(|record| record.instance_id == *instance_id),
        )
    }
}
