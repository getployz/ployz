//! The Private Access router as Docker shows it. The files it mounts are in
//! [`crate::machine_files`].

use ployz_core::labels;

use bollard::query_parameters::LogsOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::{
    Ciphertext, ContainerId, ContainerKind, ContainerObservation, MachineId, QualifiedService,
};
use zeroize::Zeroizing;

use super::{ContainerRuntime, Error};
use crate::sealed_env::{self, OpenError};

impl super::LocalDocker {
    async fn router_containers(&self) -> Result<Vec<ContainerId>, Error> {
        let identity = QualifiedService::system_tailscale();
        Ok(self
            .labelled(vec![
                labels::MANAGED.to_owned(),
                format!("{}={}", labels::NAMESPACE, identity.namespace),
                format!("{}={}", labels::SERVICE_NAME, identity.name),
            ])
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }
}

impl ContainerRuntime {
    /// Open `sealed` with this Machine's Sealing Key.
    ///
    /// # Errors
    ///
    /// Returns the [`OpenError`] when the key is missing or the auth key was sealed
    /// for another Cluster or purpose.
    pub(crate) fn open_private_access_key(
        &self,
        sealed: &Ciphertext,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        sealed_env::open_private_access_key(sealed, self.sealing_key_dir())
    }

    /// Open `sealed` with this Machine's Sealing Key.
    ///
    /// # Errors
    ///
    /// Returns the [`OpenError`] when the key is missing or the token was sealed for
    /// another Cluster.
    pub(crate) fn open_tunnel_token(
        &self,
        sealed: &Ciphertext,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        sealed_env::open_tunnel_token(sealed, self.sealing_key_dir())
    }

    /// Every Private Access router here that Docker still has and whose spec is
    /// stored, newest last.
    pub(crate) async fn routers(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let mut routers = Vec::new();
        for id in self.docker.router_containers().await? {
            let observation = match self.observation().inspect_managed(&id, machine_id).await {
                Ok(observation) => observation,
                Err(Error::ContainerNotFound(_)) => continue,
                Err(Error::SpecNotFound(id)) => {
                    tracing::warn!(container = %id, "a Private Access router has no stored spec, so the route ignores it");
                    continue;
                }
                Err(error) => return Err(error),
            };
            if observation.kind == ContainerKind::ServiceContainer {
                routers.push(observation);
            }
        }
        routers.sort_by_key(ContainerObservation::recency_key);
        Ok(routers)
    }

    /// What a router wrote since `since`, in Unix seconds, for saying why it stopped.
    /// A router refused its key restarts and repeats its start, so a fixed tail would
    /// lose the refusal. Empty when Docker cannot say.
    pub(crate) async fn router_output(&self, id: &ContainerId, since: i32) -> String {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .since(since)
            .build();
        let mut lines = self.docker.client.logs(id.as_str(), Some(options));
        let mut output = String::new();
        while let Some(Ok(chunk)) = lines.next().await {
            output.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
        }
        output
    }
}
