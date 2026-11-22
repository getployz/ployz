//! Enroll: install this Machine's first identity, as a founder or a joiner.

use ployz_core::{EnrollRequest, Enrolled, LocalMachinePhase, RecoverySeed, RpcError};

use crate::machine::{EnrollPlan, StoreError, store::Installation};

use crate::{
    docker::{ContainerRuntime, LocalDocker},
    network::{DOCKER_NETWORK_NAME, Leftover, NetworkPlane, WIREGUARD_INTERFACE_NAME},
    sealing_key::SealingKey,
};

use super::{Error, LocalMachine, departure::StorageExit};

impl LocalMachine {
    /// Enroll this Machine as a founder or member and request restart. A resend
    /// that names the stored enrollment replays it without restarting.
    ///
    /// # Errors
    ///
    /// Returns when mutation admission is busy, the record owner has stopped, the
    /// request is invalid, or storage departure fails.
    pub async fn enroll(&self, request: EnrollRequest) -> Result<Enrolled, Error> {
        self.finish_mutation(move |local| async move { local.enroll_admitted(request, None).await })
            .await
    }

    pub(super) async fn enroll_admitted(
        &self,
        request: EnrollRequest,
        recovery: Option<RecoverySeed>,
    ) -> Result<Enrolled, Error> {
        let now = self
            .approvals
            .observed_now(ployz_core::authority::Timestamp::now())?;
        // Held from the read to the spend, so no re-arm lands between them.
        let first_key = ployz_core::first_key::FirstKeyFile::beside(self.owner.data_dir());
        let held = first_key.lock().map_err(StoreError::from)?;
        let armed = held.read().map_err(StoreError::from)?;
        let plan = self
            .owner
            .mutate(move |store| store.plan_enrollment(request, recovery, armed.as_ref(), now))
            .await??;
        let machine = match plan {
            EnrollPlan::Fresh(installation) => {
                let machine = installation.machine().clone();
                let docker = self.containers.as_ref().map(ContainerRuntime::local_docker);
                let leftovers = NetworkPlane::leftovers(
                    docker.as_ref().map(LocalDocker::client),
                    self.record().private_key(),
                    &machine,
                    installation.wireguard_mtu(),
                )
                .await;
                if !leftovers.is_empty() {
                    return Ok(Enrolled::Refused {
                        error: RpcError::conflict(leftovers_refusal(&leftovers)),
                        now: self.record().now(),
                    });
                }
                self.install(installation).await?;
                held.spend(ployz_core::authority::Timestamp::now())
                    .map_err(StoreError::from)?;
                self.owner.request_restart();
                machine
            }
            EnrollPlan::Replay(machine) => machine,
            EnrollPlan::Conflict(now) => {
                return Ok(Enrolled::Refused {
                    error: RpcError::conflict(enroll_conflict(&now.phase)),
                    now,
                });
            }
        };
        tracing::info!(
            name = machine.name.as_str(),
            id = machine.id.as_str(),
            "enrolled"
        );
        let join_confirmation = match &machine.join_approval {
            Some(approval) => SealingKey::load(self.owner.data_dir())
                .map_err(Error::JoinRecord)?
                .map(|key| key.confirm_join(approval)),
            _ => None,
        };
        Ok(Enrolled::Enrolled {
            machine,
            join_confirmation,
        })
    }

    /// Install keeps every copy a prior identity left, mirrors included: an uninitialized
    /// Machine is in no Cluster, so it cannot tell whether a mirror's writer still holds
    /// what the mirror holds (#3591). Only a Leave destroys mirrors.
    async fn install(&self, installation: Installation) -> Result<(), Error> {
        self.depart_storage(StorageExit::KeepEveryCopy).await?;
        if let Some((cluster, floors)) = installation.floors() {
            self.approvals.install_floors(cluster, floors)?;
        }
        let now = self
            .approvals
            .observed_now(ployz_core::authority::Timestamp::now())?;
        self.owner
            .mutate(move |store| store.install(installation, now))
            .await??;
        Ok(())
    }
}

fn enroll_conflict(phase: &LocalMachinePhase) -> &'static str {
    match phase {
        LocalMachinePhase::Resetting => {
            "this Machine is resetting; resend Enroll once it is uninitialized"
        }
        LocalMachinePhase::Uninitialized
        | LocalMachinePhase::Joining
        | LocalMachinePhase::Participating
        | LocalMachinePhase::Unrecognized(_) => {
            "this Machine already holds a different enrollment; send Leave before enrolling it again"
        }
    }
}

/// Why a fresh enrollment is refused while an earlier enrollment's network remains,
/// with one recovery covering every leftover named.
fn leftovers_refusal(leftovers: &[Leftover]) -> String {
    let mut found = Vec::new();
    let mut steps = vec!["run `systemctl stop ployz.socket ployz`".to_owned()];
    for leftover in leftovers {
        match leftover {
            Leftover::WireGuardDevice(reason) => {
                found.push(format!(
                    "WireGuard interface {WIREGUARD_INTERFACE_NAME} remains: {reason}"
                ));
                steps.push(format!("run `ip link delete {WIREGUARD_INTERFACE_NAME}`"));
            }
            Leftover::AttachedNetwork(names) => {
                found.push(format!(
                    "Docker network {DOCKER_NETWORK_NAME} remains with containers {} attached; they belong to this Server's earlier enrollment, and removing them stops them while Volume data stays",
                    names.join(", "),
                ));
                steps.push(format!("run `docker rm -f {}`", names.join(" ")));
                steps.push(format!("run `docker network rm {DOCKER_NETWORK_NAME}`"));
            }
            Leftover::UnownedNetwork => {
                found.push(format!(
                    "Docker network {DOCKER_NETWORK_NAME} exists without the `ployzd.managed` label, so Ployz can't prove it made it"
                ));
                steps.push(format!(
                    "once nothing else needs it, run `docker network rm {DOCKER_NETWORK_NAME}`"
                ));
            }
        }
    }
    steps.extend([
        "run `systemctl reset-failed ployz.socket ployz`".to_owned(),
        "run `systemctl start ployz.socket`".to_owned(),
        "rerun the enroll command".to_owned(),
    ]);
    format!(
        "refusing to enroll over an earlier enrollment's network: {}; safe recovery: {}",
        found.join("; "),
        steps.join("; "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Leftover;

    #[test]
    fn a_refusal_names_every_leftover_and_one_recovery_for_them() {
        let message = leftovers_refusal(&[
            Leftover::WireGuardDevice(
                "the existing device identity differs from the Machine record",
            ),
            Leftover::AttachedNetwork(vec!["ployz-ingress".into(), "ployz-workspace".into()]),
        ]);
        assert_eq!(
            message,
            "refusing to enroll over an earlier enrollment's network: \
             WireGuard interface ployz-wg remains: the existing device identity differs from the Machine record; \
             Docker network ployz remains with containers ployz-ingress, ployz-workspace attached; \
             they belong to this Server's earlier enrollment, and removing them stops them while Volume data stays; \
             safe recovery: run `systemctl stop ployz.socket ployz`; run `ip link delete ployz-wg`; \
             run `docker rm -f ployz-ingress ployz-workspace`; run `docker network rm ployz`; \
             run `systemctl reset-failed ployz.socket ployz`; run `systemctl start ployz.socket`; \
             rerun the enroll command"
        );
    }

    #[test]
    fn a_refusal_gives_only_the_steps_for_what_it_found() {
        let message = leftovers_refusal(&[Leftover::WireGuardDevice(
            "the existing device identity differs from the Machine record",
        )]);
        assert!(
            message.contains("run `ip link delete ployz-wg`"),
            "{message}"
        );
        assert!(!message.contains("docker"), "{message}");
    }
}
