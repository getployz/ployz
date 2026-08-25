use ployz_core::{
    ClusterTeardown, DataLoss, DataLossConfirmation, DescribeContractRequest, MachineFailure,
    MachineSuccess, ObservedDataLoss, PartialResult, RemoveMachineRequest, RpcError,
    UnconfirmedDataLoss, derive_projects, op,
};
use tokio_util::sync::CancellationToken;

use crate::{
    cluster::{Client, evict_machine},
    connect::revoke_cloud_pairing,
    context::Transport,
    deploy::{DeploySnapshot, VolumeFate},
};

impl Client {
    /// Live Observation of Data Loss that destroying this Cluster would cause.
    ///
    /// Unions Docker Volumes across every visible Project and Machine. This is
    /// not a complete Cluster view. Mutates nothing.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn data_loss_if_cluster_destroyed(&mut self) -> Result<ObservedDataLoss, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let snapshot = self
            .deploy_snapshot(machines)
            .await
            .map_err(RpcError::from)?;
        Ok(cluster_volume_loss(&snapshot))
    }

    /// Destroy this Cluster after an exact Data Loss confirmation.
    ///
    /// Re-reads Data Loss at execute time. Confirmed identities that
    /// disappeared are ignored.
    /// User Projects are destroyed with [`VolumeFate::Destroy`]. Every Machine
    /// is reset. The Cloud Pairing is revoked when this Client is on Relay.
    /// Unreachable Machines are reported. A repeated call can finish leftover work.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the confirmation does not cover
    /// the fresh Data Loss. Unconfirmed names are in `UnconfirmedDataLoss`
    /// details. Execution is otherwise a [`ClusterTeardown`] Partial Result.
    pub async fn destroy_cluster(
        &mut self,
        confirm_data_loss: &DataLossConfirmation,
        cancellation: &CancellationToken,
    ) -> Result<ClusterTeardown, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let snapshot = self
            .deploy_snapshot(machines)
            .await
            .map_err(RpcError::from)?;
        cluster_volume_loss(&snapshot)
            .require(confirm_data_loss)
            .map_err(UnconfirmedDataLoss::into_rpc_error)?;
        let current = self
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .map_err(RpcError::from)?
            .machine_id;
        let projects = derive_projects(
            &snapshot.containers,
            snapshot
                .volume_snapshot
                .observations()
                .iter()
                .map(|volume| (&volume.id, &volume.labels)),
        );
        let mut destroyed_projects = Vec::new();
        for project in projects {
            if project.name.is_reserved() {
                continue;
            }
            match self
                .destroy_project(
                    &project.name,
                    confirm_data_loss,
                    VolumeFate::Destroy,
                    cancellation,
                    None,
                )
                .await
            {
                Ok(ployz_core::DeployOutcome::Success { .. }) => {
                    destroyed_projects.push(project.name);
                }
                Ok(ployz_core::DeployOutcome::Failed { .. }) => {}
                Err(error) if UnconfirmedDataLoss::from_rpc_error(&error).is_some() => {
                    return Err(error);
                }
                Err(_) => {}
            }
        }
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        for observation in snapshot
            .machines
            .iter()
            .filter(|observation| observation.machine.id != current)
            .chain(
                snapshot
                    .machines
                    .iter()
                    .filter(|observation| observation.machine.id == current),
            )
        {
            let machine_id = observation.machine.id;
            match evict_machine(self, observation, confirm_data_loss, current).await {
                Ok(value) => result.successes.push(MachineSuccess { machine_id, value }),
                Err(error) if UnconfirmedDataLoss::from_rpc_error(&error).is_some() => {
                    return Err(error);
                }
                Err(error) => {
                    let _ = self
                        .call::<op::RemoveMachine>(RemoveMachineRequest { machine_id }, None)
                        .await;
                    result.failures.push(MachineFailure { machine_id, error });
                }
            }
        }
        let pairing_revoked = match self.connection().transport() {
            Transport::Relay {
                url,
                credential,
                pairing,
            } => revoke_cloud_pairing(url, credential, pairing).await.is_ok(),
            Transport::Ssh { .. } | Transport::Tcp(_) | Transport::Unix(_) => false,
        };
        Ok(ClusterTeardown {
            destroyed_projects,
            machines: result,
            pairing_revoked,
        })
    }
}

fn cluster_volume_loss(snapshot: &DeploySnapshot) -> ObservedDataLoss {
    ObservedDataLoss {
        data_loss: snapshot
            .volume_snapshot
            .known_ids()
            .cloned()
            .map(DataLoss::DockerVolume)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        DockerVolumeId, DockerVolumeName, MachineId, RpcError, RpcErrorCode,
        VolumeObservationFailure,
    };

    use super::*;
    use crate::deploy::VolumeSnapshot;

    #[test]
    fn cluster_loss_includes_a_volume_whose_detail_observation_failed() {
        let id = DockerVolumeId {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            name: DockerVolumeName::parse("data").unwrap(),
        };
        let snapshot = DeploySnapshot {
            volume_snapshot: VolumeSnapshot::try_from_parts(
                Vec::new(),
                vec![VolumeObservationFailure {
                    id: id.clone(),
                    error: RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "inspect failed".into(),
                        details: serde_json::Value::Null,
                    },
                }],
                Vec::new(),
                Vec::new(),
            )
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        };

        assert_eq!(
            cluster_volume_loss(&snapshot).data_loss,
            [DataLoss::DockerVolume(id)]
        );
    }
}
