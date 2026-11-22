//! Machine admission, taken only here: the in-process turn that orders admitted work,
//! exclusive for changes and shared for image transfers and in-place updates, then the
//! installation guard that keeps an upgrade or `ployzd install` out. The turn always comes
//! first, so no two callers take the pair in different orders.
//!
//! Admitted work is detached from its RPC: a caller that stops waiting does not abandon
//! it halfway, and the task holds its guards until the work ends.

use tokio::sync::OwnedRwLockWriteGuard;

use super::{Error, LocalMachine};

/// This Machine inside work admitted to run, which no one else can build: `finish_*` hands
/// one to the work after the call passed admission, owning the handle so the work outlives
/// its RPC.
pub(crate) struct Admission(LocalMachine);

impl std::ops::Deref for Admission {
    type Target = LocalMachine;

    fn deref(&self) -> &LocalMachine {
        &self.0
    }
}

/// This Machine inside work that holds the installation guard and no turn, which only
/// [`LocalMachine::finish_installed`] makes. The work takes the turns it needs itself, so
/// a drain inside it never holds one.
pub(crate) struct Installed(LocalMachine);

impl std::ops::Deref for Installed {
    type Target = LocalMachine;

    fn deref(&self) -> &LocalMachine {
        &self.0
    }
}

impl LocalMachine {
    /// Admit work under installation exclusion only, taking no turn in Machine
    /// admission. Builds and image ingest use it alone; admitted work adds its turn.
    pub(crate) fn admit_installation(&self) -> Result<crate::mutation::MutationGuard, Error> {
        self.require_management_access()?;
        Ok(self.owner.mutation_gate().try_mutation()?)
    }

    /// Run `work` alone: no other admitted work runs on this Machine meanwhile.
    pub(super) async fn finish_mutation<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Admission) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let exclusive = self.exclusive_turn().await;
        self.finish_admitted(exclusive, work).await
    }

    /// Run `work` beside other shared work, while no exclusive work runs.
    pub(super) async fn finish_shared<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Admission) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let shared = self.owner.admission_lock().read_owned().await;
        self.finish_admitted(shared, work).await
    }

    async fn finish_admitted<T, F, Fut, G>(&self, turn: G, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Admission) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
        G: Send + 'static,
    {
        let installation = self.admit_installation()?;
        let admission = Admission(self.clone());
        tokio::spawn(async move {
            let _held = (turn, installation);
            work(admission).await
        })
        .await?
    }

    /// Run `work` holding the installation guard and no turn.
    pub(super) async fn finish_installed<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Installed) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let installation = self.admit_installation()?;
        let installed = Installed(self.clone());
        tokio::spawn(async move {
            let _installation = installation;
            work(installed).await
        })
        .await?
    }

    /// The exclusive admission turn, for work that takes it itself. Work holding one never
    /// asks for the installation guard after it, since only `finish_*` takes that.
    pub(super) async fn exclusive_turn(&self) -> OwnedRwLockWriteGuard<()> {
        self.owner.admission_lock().write_owned().await
    }

    /// Wait until no admitted work holds a turn.
    pub(super) async fn wait_for_admitted_work(&self) {
        drop(self.exclusive_turn().await);
    }

    /// The exclusive turn and the installation itself, taken only when both are free now,
    /// for an upgrade that replaces this daemon.
    ///
    /// # Errors
    /// Returns [`crate::mutation::Error::Busy`] when admitted work holds a turn,
    /// [`Error::ManagementRevoked`] for a revoked management client, and the gate's
    /// refusal when an installation or mutation is active.
    pub(super) fn try_exclusive_installation(
        &self,
    ) -> Result<
        (
            OwnedRwLockWriteGuard<()>,
            crate::mutation::InstallationGuard,
        ),
        Error,
    > {
        let turn = self
            .owner
            .admission_lock()
            .try_write_owned()
            .map_err(|_| crate::mutation::Error::Busy)?;
        self.require_management_access()?;
        let installation = self.owner.mutation_gate().try_installation()?;
        Ok((turn, installation))
    }

    #[cfg(test)]
    pub(crate) async fn hold_mutation_for_test(
        &self,
        started: std::sync::Arc<tokio::sync::Notify>,
        release: std::sync::Arc<tokio::sync::Notify>,
    ) -> Result<(), Error> {
        self.finish_mutation(move |_| async move {
            started.notify_one();
            release.notified().await;
            Ok(())
        })
        .await
    }
}
