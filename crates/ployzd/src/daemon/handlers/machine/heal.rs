use std::collections::BTreeMap;

use ipnet::Ipv4Net;
use ployz_orchestrator::Phase;
use ployz_orchestrator::mesh::tasks::ParticipationCommand;
use ployz_runtime_api::RestartableWorkload;
use ployz_store_api::{MachineStore, StoreRuntimeControl};
use ployz_state::store::network::NetworkConfig;
use ployz_state::time::now_unix_secs;
use ployz_types::model::{MachineId, MachineRecord, MachineStatus, Participation};

use crate::daemon::{DaemonState, PendingSubnetHeal, SubnetHealAttempt};

use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use super::types::{LocalSubnetConflict, LocalSubnetHealPlan};

const SUBNET_HEAL_COOLDOWN_SECS: u64 = 10;
const SUBNET_HEAL_SETTLE_SECS: u64 = 10;

impl DaemonState {
    pub async fn heal_local_subnet_conflict_if_needed(&mut self) {
        let Some(active) = self.active.as_ref() else {
            self.pending_subnet_heal = None;
            self.last_subnet_heal_attempt = None;
            return;
        };

        if active.mesh.phase() != Phase::Running {
            return;
        }

        if !active.mesh.store.healthy().await {
            tracing::info!(
                machine_id = %self.identity.machine_id,
                "local subnet heal: store unhealthy, deferring"
            );
            return;
        }

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => {
                tracing::warn!(error = %err, "local subnet heal: failed to list machines");
                return;
            }
        };

        if !machines
            .iter()
            .any(|machine| machine.id == self.identity.machine_id)
        {
            tracing::warn!(
                machine_id = %self.identity.machine_id,
                "local subnet heal: local machine missing from store, deferring"
            );
            return;
        }

        let duplicate_groups = duplicate_subnet_groups(&machines);
        let active_subnet = active.config.subnet;
        let current_conflict =
            local_duplicate_subnet_conflict(&machines, &self.identity.machine_id);
        let healing_in_progress = self.pending_subnet_heal.is_some_and(|pending| {
            pending.network_subnet == active_subnet && pending.target_subnet != active_subnet
        });

        if duplicate_groups.is_empty() && !healing_in_progress {
            if let Err(err) = self.set_local_participation_override(None).await {
                tracing::warn!(error = %err, "local subnet heal: failed to clear participation override");
            }
            self.pending_subnet_heal = None;
            self.last_subnet_heal_attempt = None;
            return;
        }

        if !duplicate_groups.is_empty() {
            tracing::warn!(
                machine_id = %self.identity.machine_id,
                duplicate_groups = ?duplicate_group_log_view(&duplicate_groups),
                "local subnet heal: duplicate subnet claims detected"
            );
        }

        let now = now_unix_secs();
        let pending = match self.pending_subnet_heal {
            Some(pending)
                if pending.network_subnet == active_subnet
                    && pending.target_subnet != active_subnet =>
            {
                pending
            }
            _ => {
                let Some(current_conflict) = current_conflict else {
                    if let Err(err) = self.set_local_participation_override(None).await {
                        tracing::warn!(error = %err, "local subnet heal: failed to clear participation override");
                    }
                    self.pending_subnet_heal = None;
                    return;
                };
                let target_subnet = match allocate_replacement_subnet(
                    &machines,
                    &self.identity.machine_id,
                    &self.cluster_cidr,
                    self.subnet_prefix_len,
                ) {
                    Ok(target_subnet) => target_subnet,
                    Err(err) => {
                        tracing::warn!(error = %err, "local subnet heal: failed to plan subnet heal");
                        return;
                    }
                };
                if let Err(err) = self.reserve_local_subnet_claim(target_subnet).await {
                    tracing::warn!(error = %err, "local subnet heal: failed to reserve replacement subnet");
                    return;
                }
                let pending = PendingSubnetHeal {
                    network_subnet: current_conflict.subnet,
                    target_subnet,
                    planned_at: now,
                };
                self.pending_subnet_heal = Some(pending);
                tracing::info!(
                    machine_id = %self.identity.machine_id,
                    current_subnet = %pending.network_subnet,
                    target_subnet = %pending.target_subnet,
                    settle_secs = SUBNET_HEAL_SETTLE_SECS,
                    "local subnet heal: planned replacement subnet, waiting for settle window"
                );
                return;
            }
        };

        let target_winner =
            target_subnet_winner(&machines, &self.identity.machine_id, pending.target_subnet);
        if target_winner != self.identity.machine_id {
            let target_subnet = match allocate_replacement_subnet(
                &machines,
                &self.identity.machine_id,
                &self.cluster_cidr,
                self.subnet_prefix_len,
            ) {
                Ok(target_subnet) => target_subnet,
                Err(err) => {
                    tracing::warn!(error = %err, "local subnet heal: failed to replan subnet heal");
                    return;
                }
            };
            if let Err(err) = self.reserve_local_subnet_claim(target_subnet).await {
                tracing::warn!(error = %err, "local subnet heal: failed to reserve replanned subnet");
                return;
            }
            let pending = PendingSubnetHeal {
                network_subnet: active_subnet,
                target_subnet,
                planned_at: now,
            };
            self.pending_subnet_heal = Some(pending);
            tracing::info!(
                machine_id = %self.identity.machine_id,
                losing_machine_id = %target_winner,
                current_subnet = %pending.network_subnet,
                target_subnet = %pending.target_subnet,
                settle_secs = SUBNET_HEAL_SETTLE_SECS,
                "local subnet heal: lost target subnet tie-break, replanning"
            );
            return;
        }

        if now.saturating_sub(pending.planned_at) < SUBNET_HEAL_SETTLE_SECS {
            tracing::info!(
                machine_id = %self.identity.machine_id,
                current_subnet = %pending.network_subnet,
                target_subnet = %pending.target_subnet,
                settle_secs = SUBNET_HEAL_SETTLE_SECS,
                "local subnet heal: waiting for settle window"
            );
            return;
        }

        let plan = LocalSubnetHealPlan {
            current_subnet: active_subnet,
            winner_machine_id: subnet_claim_winner(&machines, active_subnet)
                .unwrap_or_else(|| self.identity.machine_id.clone()),
            target_subnet: pending.target_subnet,
        };

        if let Some(last_attempt) = self.last_subnet_heal_attempt
            && last_attempt.network_subnet == plan.current_subnet
            && last_attempt.target_subnet == plan.target_subnet
            && now.saturating_sub(last_attempt.attempted_at) < SUBNET_HEAL_COOLDOWN_SECS
        {
            tracing::info!(
                machine_id = %self.identity.machine_id,
                current_subnet = %plan.current_subnet,
                target_subnet = %plan.target_subnet,
                "local subnet heal: skipping repeated attempt during cooldown"
            );
            return;
        }

        self.pending_subnet_heal = None;
        self.last_subnet_heal_attempt = Some(SubnetHealAttempt {
            network_subnet: plan.current_subnet,
            target_subnet: plan.target_subnet,
            attempted_at: now,
        });

        let operation_store = self.machine_operation_store();
        let mut operation = match operation_store.begin(
            MachineOperationKind::Heal,
            self.active
                .as_ref()
                .map(|active| active.config.name.0.clone()),
            Vec::new(),
            "apply-local-subnet-heal",
            MachineOperationArtifacts {
                machine_id: Some(self.identity.machine_id.clone()),
                allocated_subnet: Some(plan.target_subnet.to_string()),
                ..MachineOperationArtifacts::default()
            },
        ) {
            Ok(operation) => Some(operation),
            Err(err) => {
                tracing::warn!(error = %err, "local subnet heal: failed to persist operation start");
                None
            }
        };

        tracing::warn!(
            machine_id = %self.identity.machine_id,
            winner_machine_id = %plan.winner_machine_id,
            current_subnet = %plan.current_subnet,
            target_subnet = %plan.target_subnet,
            "local subnet heal: starting"
        );

        match self.apply_local_subnet_heal(&plan).await {
            Ok(()) => {
                if let Some(ref mut operation) = operation {
                    let _ = operation_store.update_status(
                        operation,
                        MachineOperationStatus::Succeeded,
                        None,
                    );
                }
                tracing::info!(
                    machine_id = %self.identity.machine_id,
                    current_subnet = %plan.current_subnet,
                    target_subnet = %plan.target_subnet,
                    "local subnet heal: complete"
                );
            }
            Err(err) => {
                if let Some(ref mut operation) = operation {
                    let _ = operation_store.update_status(
                        operation,
                        MachineOperationStatus::Failed,
                        Some(err.clone()),
                    );
                }
                tracing::warn!(
                    machine_id = %self.identity.machine_id,
                    current_subnet = %plan.current_subnet,
                    target_subnet = %plan.target_subnet,
                    error = %err,
                    "local subnet heal: failed"
                );
            }
        }
    }

    async fn reserve_local_subnet_claim(&self, target_subnet: Ipv4Net) -> Result<(), String> {
        let Some(active) = self.active.as_ref() else {
            return Err("no running network".into());
        };

        let now = now_unix_secs();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.subnet = Some(target_subnet);
                record.participation = Participation::Disabled;
                record.updated_at = now;
            })
            .await
        else {
            return Err("local authoritative self record missing".into());
        };

        active
            .mesh
            .store
            .upsert_self_machine(&record)
            .await
            .map_err(|err| format!("reserve local subnet claim: {err}"))?;

        self.set_local_participation_override(Some(Participation::Disabled))
            .await
    }

    async fn apply_local_subnet_heal(&mut self, plan: &LocalSubnetHealPlan) -> Result<(), String> {
        let network_name = self
            .active
            .as_ref()
            .map(|active| active.config.name.0.clone())
            .ok_or_else(|| "no running network".to_string())?;
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        let mut config = NetworkConfig::load(&config_path)
            .map_err(|err| format!("load network config: {err}"))?;
        config.subnet = plan.target_subnet;
        config
            .save(&config_path)
            .map_err(|err| format!("save network config: {err}"))?;

        if self.runtime_ops.is_memory_test() {
            self.apply_local_subnet_heal_in_memory_mode(&network_name)
                .await
        } else {
            self.restart_active_mesh_for_subnet_heal(&network_name, plan.target_subnet)
                .await
        }
    }

    async fn apply_local_subnet_heal_in_memory_mode(
        &mut self,
        network_name: &str,
    ) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network_name);
        let config = NetworkConfig::load(&config_path)
            .map_err(|err| format!("load network config: {err}"))?;
        let Some(active) = self.active.as_mut() else {
            return Err("no running network".into());
        };

        active.config = config.clone();
        let Some(mut record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.subnet = Some(config.subnet);
                record.updated_at = now_unix_secs();
            })
            .await
        else {
            return Err("local authoritative self record missing".into());
        };
        record.subnet = Some(config.subnet);
        active
            .mesh
            .store
            .upsert_self_machine(&record)
            .await
            .map_err(|err| format!("update healed local machine record: {err}"))?;

        self.set_local_participation_override(None).await
    }

    async fn set_local_participation_override(
        &self,
        participation: Option<Participation>,
    ) -> Result<(), String> {
        let Some(active) = self.active.as_ref() else {
            return Err("no running network".into());
        };
        let Some(participation_tx) = active.mesh.participation_sender() else {
            return Ok(());
        };
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        participation_tx
            .send(ParticipationCommand::SetForced {
                participation,
                done: done_tx,
            })
            .await
            .map_err(|err| format!("send participation override: {err}"))?;
        done_rx
            .await
            .map_err(|err| format!("wait participation override ack: {err}"))
    }

    async fn restart_active_mesh_for_subnet_heal(
        &mut self,
        network_name: &str,
        claimed_subnet: Ipv4Net,
    ) -> Result<(), String> {
        if self.active.is_none() {
            return Err("no running network".into());
        }

        self.publish_local_machine_down_for_subnet_heal(claimed_subnet)
            .await?;

        let workloads = self
            .stop_local_workloads_for_subnet_heal(network_name)
            .await?;

        if let Err(error) = self
            .restart_active_runtime_for_subnet_heal(network_name)
            .await
        {
            return Err(error);
        }

        self.start_local_workloads_after_subnet_heal(network_name, &workloads)
            .await
    }

    async fn publish_local_machine_down_for_subnet_heal(
        &self,
        claimed_subnet: Ipv4Net,
    ) -> Result<(), String> {
        let Some(active) = self.active.as_ref() else {
            return Err("no running network".into());
        };

        let now = now_unix_secs();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.subnet = Some(claimed_subnet);
                record.status = MachineStatus::Down;
                record.participation = Participation::Disabled;
                record.last_heartbeat = now;
                record.updated_at = now;
            })
            .await
        else {
            return Err("local authoritative self record missing".into());
        };
        active
            .mesh
            .store
            .upsert_self_machine(&record)
            .await
            .map_err(|err| format!("mark local machine down before subnet heal: {err}"))
    }

    async fn stop_local_workloads_for_subnet_heal(
        &self,
        network_name: &str,
    ) -> Result<Vec<RestartableWorkload>, String> {
        let target_subnet = self
            .active
            .as_ref()
            .map(|active| active.config.subnet)
            .ok_or_else(|| "no running network".to_string())?;
        self.runtime_ops
            .stop_local_workloads_for_subnet_heal(
                &self.identity.machine_id,
                network_name,
                target_subnet,
            )
            .await
    }

    async fn start_local_workloads_after_subnet_heal(
        &self,
        network_name: &str,
        workloads: &[RestartableWorkload],
    ) -> Result<(), String> {
        let target_subnet = self
            .active
            .as_ref()
            .map(|active| active.config.subnet)
            .ok_or_else(|| "no running network".to_string())?;
        self.runtime_ops
            .start_local_workloads_after_subnet_heal(network_name, target_subnet, workloads)
            .await
    }
}

pub(super) fn duplicate_subnet_groups(
    machines: &[MachineRecord],
) -> Vec<(Ipv4Net, Vec<MachineId>)> {
    let mut groups: BTreeMap<Ipv4Net, Vec<MachineId>> = BTreeMap::new();

    for machine in machines {
        let Some(subnet) = machine.subnet else {
            continue;
        };
        groups.entry(subnet).or_default().push(machine.id.clone());
    }

    groups
        .into_iter()
        .filter_map(|(subnet, mut machine_ids)| {
            if machine_ids.len() < 2 {
                return None;
            }
            machine_ids.sort_by(|left, right| left.0.cmp(&right.0));
            Some((subnet, machine_ids))
        })
        .collect()
}

fn duplicate_group_log_view(groups: &[(Ipv4Net, Vec<MachineId>)]) -> Vec<(String, Vec<String>)> {
    groups
        .iter()
        .map(|(subnet, machine_ids)| {
            (
                subnet.to_string(),
                machine_ids
                    .iter()
                    .map(|machine_id| machine_id.0.clone())
                    .collect(),
            )
        })
        .collect()
}

fn local_duplicate_subnet_conflict(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
) -> Option<LocalSubnetConflict> {
    for (subnet, machine_ids) in duplicate_subnet_groups(machines) {
        let [winner_machine_id, ..] = machine_ids.as_slice() else {
            continue;
        };
        if winner_machine_id == local_machine_id {
            return None;
        }
        if machine_ids
            .iter()
            .any(|machine_id| machine_id == local_machine_id)
        {
            return Some(LocalSubnetConflict {
                subnet,
                winner_machine_id: winner_machine_id.clone(),
            });
        }
    }

    None
}

fn target_subnet_winner(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    target_subnet: Ipv4Net,
) -> MachineId {
    let mut contenders: Vec<MachineId> = machines
        .iter()
        .filter(|machine| machine.subnet == Some(target_subnet))
        .map(|machine| machine.id.clone())
        .collect();
    contenders.push(local_machine_id.clone());
    contenders.sort_by(|left, right| left.0.cmp(&right.0));
    let [winner, ..] = contenders.as_slice() else {
        return local_machine_id.clone();
    };
    winner.clone()
}

fn subnet_claim_winner(machines: &[MachineRecord], subnet: Ipv4Net) -> Option<MachineId> {
    let mut contenders: Vec<MachineId> = machines
        .iter()
        .filter(|machine| machine.subnet == Some(subnet))
        .map(|machine| machine.id.clone())
        .collect();
    contenders.sort_by(|left, right| left.0.cmp(&right.0));
    let [winner, ..] = contenders.as_slice() else {
        return None;
    };
    Some(winner.clone())
}

#[cfg(test)]
pub(super) fn plan_local_subnet_heal(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    cluster_cidr: &str,
    subnet_prefix_len: u8,
) -> Result<Option<LocalSubnetHealPlan>, String> {
    for (subnet, machine_ids) in duplicate_subnet_groups(machines) {
        let [winner_machine_id, ..] = machine_ids.as_slice() else {
            continue;
        };
        if winner_machine_id == local_machine_id {
            return Ok(None);
        }
        if !machine_ids
            .iter()
            .any(|machine_id| machine_id == local_machine_id)
        {
            continue;
        }

        let target_subnet = allocate_replacement_subnet(
            machines,
            local_machine_id,
            cluster_cidr,
            subnet_prefix_len,
        )?;
        return Ok(Some(LocalSubnetHealPlan {
            current_subnet: subnet,
            winner_machine_id: winner_machine_id.clone(),
            target_subnet,
        }));
    }

    Ok(None)
}

fn allocate_replacement_subnet(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    cluster_cidr: &str,
    subnet_prefix_len: u8,
) -> Result<Ipv4Net, String> {
    let cluster: Ipv4Net = cluster_cidr
        .parse()
        .map_err(|err| format!("invalid cluster CIDR '{cluster_cidr}': {err}"))?;
    let allocated = machines.iter().filter_map(|machine| {
        if machine.id == *local_machine_id {
            return None;
        }
        machine.subnet
    });
    let mut ipam = ployz_state::network::ipam::Ipam::with_allocated(
        cluster,
        subnet_prefix_len,
        allocated,
    );
    ipam.allocate()
        .ok_or_else(|| "no available subnets for local heal".into())
}
