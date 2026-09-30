use ployz_core::{
    ClusterTeardown, DataLoss, DataLossConfirmation, DescribeContractRequest, MachineFailure,
    MachineSuccess, ObservedDataLoss, PartialResult, RemoveMachineRequest, RpcError,
    UnconfirmedDataLoss, derive_namespaces, op,
};
use tokio_util::sync::CancellationToken;

use crate::{
    cluster::{Client, evict_machine},
    deploy::{DeploySnapshot, VolumeFate},
    output::Gaps,
};

impl Client {
    /// Every user Namespace the Cluster runs, with its Services and Docker Volumes,
    /// as far as the Machines that answered show it, and the Machines whose
    /// Containers or Volumes went unseen.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub(crate) async fn namespaces(
        &mut self,
    ) -> Result<(Vec<ployz_core::NamespaceObservation>, Gaps), RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let snapshot = self
            .deploy_snapshot(machines)
            .await
            .map_err(RpcError::from)?;
        let seen = &snapshot.volume_snapshot;
        let mut gaps = Gaps::default();
        gaps.extend(&snapshot.container_failures, &snapshot.container_omissions);
        gaps.extend(seen.machine_failures(), seen.omissions());
        // A Volume a Machine couldn't inspect has no known Namespace.
        let unread: Vec<_> = seen
            .named_failures()
            .iter()
            .map(|failure| MachineFailure {
                machine_id: failure.id.machine_id,
                error: failure.error.clone(),
            })
            .collect();
        gaps.extend(&unread, &[]);
        let namespaces = derive_namespaces(
            &snapshot.containers,
            seen.observations()
                .iter()
                .map(|volume| (&volume.id, &volume.labels)),
        )
        .into_iter()
        .filter(|namespace| !namespace.name.is_reserved())
        .collect();
        Ok((namespaces, gaps))
    }

    /// Live Observation of Data Loss that destroying this Cluster would cause.
    ///
    /// Unions Docker Volumes across every visible Namespace and Machine. This is
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
    /// User Namespaces are destroyed with [`VolumeFate::Destroy`]. Every Machine
    /// is reset. Endpoint revocation is confirmed separately by Cloud.
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
        let namespaces = derive_namespaces(
            &snapshot.containers,
            snapshot
                .volume_snapshot
                .observations()
                .iter()
                .map(|volume| (&volume.id, &volume.labels)),
        );
        let mut destroyed_namespaces = Vec::new();
        for namespace in namespaces {
            if namespace.name.is_reserved() {
                continue;
            }
            match self
                .destroy_namespace(
                    &namespace.name,
                    confirm_data_loss,
                    VolumeFate::Destroy,
                    cancellation,
                    None,
                )
                .await
            {
                Ok(ployz_core::DeployOutcome::Success { .. }) => {
                    destroyed_namespaces.push(namespace.name);
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
        Ok(ClusterTeardown {
            destroyed_namespaces,
            machines: result,
            pairing_revoked: false,
        })
    }
}

fn cluster_volume_loss(snapshot: &DeploySnapshot) -> ObservedDataLoss {
    ObservedDataLoss {
        data_loss: snapshot
            .volume_snapshot
            .known_ids()
            .cloned()
            .map(|id| DataLoss::DockerVolume { id })
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
            [DataLoss::DockerVolume { id }]
        );
    }
}
