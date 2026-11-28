//! Departure: Leave, a member's Forget, and what each does with this Machine's storage.

use ployz_core::{
    ForgetRequest, Forgotten, LeaveRequest, Left, LeftPhase, LocalMachinePhase, RpcError,
    RpcErrorCode, StorageDeparture,
};

use crate::machine::{RecordOwner, store::PreparedReset};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Forget another Machine: record it here as removed, then delete its replicated row. The
    /// local record comes first, so once a Forget answers, this Machine refuses the
    /// Machine's join and its row even before the delete reaches its store. A missing row
    /// is success.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClusterStoreUnavailable`] when the Cluster store is
    /// missing, [`Error::RecordOwner`] or [`Error::Store`] when the removal can't be
    /// recorded, and [`Error::Cluster`] when the replicated delete fails.
    pub async fn forget(&self, request: ForgetRequest) -> Result<Forgotten, Error> {
        self.finish_mutation(move |local| async move {
            let now = local.record().now();
            if now.id == request.machine_id {
                return Ok(Forgotten::Refused {
                    error: refusal(
                        RpcErrorCode::InvalidArgument,
                        "this is the Machine itself; send Leave to it instead",
                    ),
                    now,
                });
            }
            let replicated = local.replicated()?;
            let public_key = match replicated.machine(request.machine_id.as_str()).await? {
                Some(row) => Some(row.public_key),
                None => local.joining_key(request.machine_id)?,
            };
            let removed = crate::machine::RemovedMember {
                id: request.machine_id,
                public_key,
            };
            local
                .owner
                .mutate(move |store| store.remove_members(&[removed]))
                .await??;
            replicated.forget_machine(&request.machine_id).await?;
            Ok(Forgotten::Forgotten)
        })
        .await
    }

    /// Leave the Cluster: clean managed containers, depart storage, persist the
    /// reset, best-effort delete this Machine's replicated row, and restart the
    /// daemon into the reset. Once admitted, the leave continues if the caller
    /// disconnects. A resend on a Machine already resetting repeats the cleanup.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Upgrade`] when the upgrade receipt cannot be read,
    /// [`Error::Cleanup`] when managed container removal fails,
    /// [`Error::RecordOwner`] when the record owner has stopped,
    /// [`Error::Store`] when reset cannot be prepared or committed, and
    /// [`Error::OperationTask`] if the admitted operation task fails.
    pub async fn leave(&self, _: LeaveRequest) -> Result<Left, Error> {
        self.depart(StorageExit::Depart).await
    }

    async fn depart(&self, exit: StorageExit) -> Result<Left, Error> {
        if self.record().phase() == LocalMachinePhase::Uninitialized {
            return Ok(Left::Left {
                phase: LeftPhase::Uninitialized,
                reset_warning: None,
            });
        }
        match crate::installer::upgrade::latest(self.owner.data_dir(), self.owner.run_dir()).await {
            Ok(Some(attempt)) if !attempt.is_terminal() => {
                return Ok(Left::Refused {
                    error: refusal(
                        RpcErrorCode::Conflict,
                        "an upgrade is running on this Machine; resend Leave once it finishes",
                    ),
                    now: self.record().now(),
                });
            }
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        self.finish_mutation(move |local| async move { local.leave_admitted(exit).await })
            .await
    }

    async fn leave_admitted(&self, exit: StorageExit) -> Result<Left, Error> {
        let machine_id = self.record().id();
        let publication = match &self.cluster {
            Some(cluster) => Some(cluster.replicated.machine_publication().await),
            None => None,
        };
        // From here until the row goes, a deploy plans no Ingress Proxy here and the
        // ingress check doesn't call this Machine unchecked. Best effort: a Machine
        // whose Cluster can't take the mark still leaves.
        if let Some(publication) = &publication
            && let Err(error) = publication.mark_leaving(&machine_id, true).await
        {
            tracing::warn!(%error, "could not mark this Machine's row as leaving");
        }
        if let Err(error) = self.depart_admitted(exit).await {
            if let Some(publication) = &publication
                && let Err(error) = publication.mark_leaving(&machine_id, false).await
            {
                tracing::warn!(%error, "could not unmark this Machine's row after a failed Leave");
            }
            return Err(error);
        }
        let reset_warning = match publication {
            Some(publication) => publication
                .remove(&machine_id)
                .await
                .err()
                .map(|error| ployz_core::error_chain::inline(&error)),
            None => None,
        };
        Ok(leave_response(&self.owner, reset_warning))
    }

    /// Reset this Machine: its Containers, its storage, then its phase. Any failure
    /// leaves it Participating.
    async fn depart_admitted(&self, exit: StorageExit) -> Result<(), Error> {
        let prepared_reset = self
            .owner
            .mutate(|store| {
                if store.record().phase() == LocalMachinePhase::Resetting {
                    Ok(None)
                } else {
                    store.prepare_reset().map(Some)
                }
            })
            .await??;
        // A resend on a Machine already resetting leaves it leaving whatever happens.
        let began_reset = prepared_reset.is_some();
        let departed = self.clear_out(prepared_reset, exit).await;
        if departed.is_err()
            && began_reset
            && let Some(containers) = &self.containers
        {
            containers.lifecycle().departure_failed();
        }
        departed
    }

    /// Remove every managed Container, depart storage and commit the reset.
    async fn clear_out(
        &self,
        prepared_reset: Option<PreparedReset>,
        exit: StorageExit,
    ) -> Result<(), Error> {
        if let Some(containers) = &self.containers
            && let Err(error) = containers.lifecycle().remove_all_managed().await
        {
            return Err(Error::Cleanup(ployz_core::error_chain::inline(&error)));
        }
        self.depart_storage(exit).await?;
        if let Some(prepared_reset) = prepared_reset {
            self.owner
                .mutate(move |store| prepared_reset.commit(store))
                .await??;
        }
        Ok(())
    }

    /// Demote every root through the plugin, make Docker forget each demoted name, then
    /// forget every Volume floor, so a lane from a new cluster starts at any `gen`.
    /// A Machine without Docker has no volume driver and nothing to depart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cleanup`] when the plugin cannot demote, a Volume's copy would not
    /// stop or its demote failed, or Docker keeps a name.
    pub(super) async fn depart_storage(&self, exit: StorageExit) -> Result<(), Error> {
        let Some(containers) = &self.containers else {
            return Ok(());
        };
        let departure: StorageDeparture = self
            .plugin
            .call(exit.route(), &())
            .await
            .map_err(|error| Error::Cleanup(format!("storage departure failed: {error}")))?;
        // A retry answers only what it changes, so the demoted are forgotten now even
        // when a stuck Volume or another kept name fails this departure.
        let mut left = Vec::new();
        for name in &departure.demoted {
            if let Err(error) = containers.volumes().forget_volume(name).await {
                left.push(format!("Docker kept Volume {name}: {error}"));
            }
        }
        left.extend(
            departure
                .stuck
                .iter()
                .map(|name| format!("Volume {name} did not stop copying")),
        );
        left.extend(
            departure
                .failed
                .iter()
                .map(|failed| format!("Volume {} did not depart: {}", failed.name, failed.reason)),
        );
        if !left.is_empty() {
            return Err(Error::Cleanup(format!(
                "{}; retry the departure",
                left.join("; ")
            )));
        }
        containers
            .lifecycle()
            .clear_floors()
            .await
            .map_err(|error| {
                Error::Cleanup(format!(
                    "the Volume floors stayed: {}",
                    ployz_core::error_chain::inline(&error)
                ))
            })
    }
}

/// What a departure does with this Machine's mirrors: a leave destroys those a writer
/// elsewhere holds; a fresh install keeps every copy.
#[derive(Clone, Copy)]
pub(super) enum StorageExit {
    Depart,
    KeepEveryCopy,
}

impl StorageExit {
    fn route(self) -> &'static str {
        match self {
            Self::Depart => "Storage.Demote",
            Self::KeepEveryCopy => "Storage.Seal",
        }
    }
}

fn leave_response(owner: &RecordOwner, reset_warning: Option<String>) -> Left {
    owner.request_restart();
    Left::Left {
        phase: LeftPhase::Resetting,
        reset_warning,
    }
}

fn refusal(code: RpcErrorCode, message: &str) -> RpcError {
    RpcError::new(code, message)
}

#[cfg(test)]
#[path = "departure_tests.rs"]
mod departure_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> (tempfile::TempDir, crate::machine::RecordOwner) {
        let dir = tempfile::tempdir().unwrap();
        let owner = crate::machine::RecordOwner::spawn(
            crate::machine::LocalMachineStore::open(dir.path()).unwrap(),
        )
        .unwrap();
        (dir, owner)
    }

    #[test]
    fn leave_with_a_publication_warning_still_restarts() {
        let (_dir, owner) = owner();
        let restart_rx = owner.restart_requested();
        let left = leave_response(&owner, Some("replicated delete failed".into()));

        assert_eq!(
            left,
            ployz_core::Left::Left {
                phase: ployz_core::LeftPhase::Resetting,
                reset_warning: Some("replicated delete failed".into()),
            }
        );
        assert!(*restart_rx.borrow());
    }
}
