//! Machine admission, taken only here: the in-process turn that orders admitted work,
//! exclusive for changes and shared for image transfers, in-place updates, policy
//! edits and the stop of a copy out of routing, then the installation guard that
//! keeps an upgrade or `ployzd install` out. The turn always comes first, and the switch,
//! when work takes it, before both, as [`super::switch`] sets out, so no two callers take
//! them in different orders.
//!
//! Admitted work is detached from its RPC: a caller that stops waiting does not abandon
//! it halfway, and the task holds its guards until the work ends. Work run through
//! [`LocalMachine::finish_shared_with_caller`] is the one exception: it learns when its
//! caller stops waiting, and a peer image transfer not yet tagging then stops. The sweep
//! of its staging names runs off the turn.
//!
//! Each turn admitted work acts under checks the caller's access once taken, against the
//! keys as they stand, and the work acts at the scope that check found: removing a key
//! takes the switch and its own exclusive turn, so a caller removed while its work waited
//! is refused before any effect, and one that lost only its Device slot acts as an
//! Operator.

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard};

use super::{Checked, Error, LocalMachine};

/// This Machine inside work admitted to run, which no one else can build: `finish_*` hands
/// one to the work after the call passed admission under its turn, at the scope its caller
/// holds then, owning the handle so the work outlives its RPC.
pub(crate) struct Admission(Checked);

impl std::ops::Deref for Admission {
    type Target = Checked;

    fn deref(&self) -> &Checked {
        &self.0
    }
}

/// This Machine inside work that holds the installation guard and no turn, which only
/// [`LocalMachine::finish_installed`] makes. The work takes the turns it needs itself, so
/// a drain inside it never holds one, and takes each through [`Installed::exclusive_turn`]
/// or [`Installed::shared_turn`]. It reads no scope until then.
pub(crate) struct Installed(LocalMachine);

/// The exclusive admission turn, held by work [`LocalMachine::finish_installed`] runs,
/// and this Machine serving the call at the access its caller holds under it. Only
/// [`Installed::exclusive_turn`] makes one, so an effect that takes one never runs for a
/// caller removed while the work waited, nor at a scope the caller lost meanwhile.
pub(crate) struct ExclusiveTurn {
    local: Checked,
    _turn: OwnedRwLockWriteGuard<()>,
}

/// The shared admission turn, held by work [`LocalMachine::finish_installed`] runs beside
/// other shared work. Only [`Installed::shared_turn`] makes one, after the same check as
/// [`ExclusiveTurn`].
pub(crate) struct SharedTurn {
    _turn: OwnedRwLockReadGuard<()>,
}

impl std::ops::Deref for Installed {
    type Target = LocalMachine;

    fn deref(&self) -> &LocalMachine {
        &self.0
    }
}

impl std::ops::Deref for ExclusiveTurn {
    type Target = Checked;

    fn deref(&self) -> &Checked {
        &self.local
    }
}

impl Installed {
    /// The exclusive admission turn, with the caller's access checked under it.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] when this Machine no longer accepts the
    /// caller's key.
    pub(crate) async fn exclusive_turn(&self) -> Result<ExclusiveTurn, Error> {
        let turn = self.0.unchecked_exclusive_turn().await;
        Ok(ExclusiveTurn {
            local: self.0.clone().checked_under(&turn)?,
            _turn: turn,
        })
    }

    /// The shared admission turn, with the caller's access checked under it.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] when this Machine no longer accepts the
    /// caller's key.
    pub(crate) async fn shared_turn(&self) -> Result<SharedTurn, Error> {
        let turn = self.0.owner.admission_lock().read_owned().await;
        self.0.clone().checked_under(&turn)?;
        Ok(SharedTurn { _turn: turn })
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
        let exclusive = self.unchecked_exclusive_turn().await;
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

    /// [`Self::finish_shared`], handing `work` a token cancelled once this call's caller
    /// stops waiting, so work that stops on it frees its turn for the next caller.
    pub(super) async fn finish_shared_with_caller<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Admission, tokio_util::sync::CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let gone = tokio_util::sync::CancellationToken::new();
        let _caller = gone.clone().drop_guard();
        self.finish_shared(move |local| work(local, gone)).await
    }

    async fn finish_admitted<T, F, Fut, G>(&self, turn: G, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Admission) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
        G: super::call::Held + Send + 'static,
    {
        let local = self.clone().checked_under(&turn)?;
        let installation = local.admit_installation()?;
        let admission = Admission(local);
        tokio::spawn(async move {
            let _held = (turn, installation);
            work(admission).await
        })
        .await?
    }

    /// Run `work` holding the installation guard and no turn. The caller's access is
    /// checked now, so a removed caller drains nothing, and again under each turn the work
    /// takes, which serves the call at the scope that check finds.
    pub(super) async fn finish_installed<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Installed) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let installation = self.admit_installation()?;
        let running = self.owner.installed_work().read_owned().await;
        let installed = Installed(self.clone());
        tokio::spawn(async move {
            let _held = (installation, running);
            work(installed).await
        })
        .await?
    }

    /// The exclusive admission turn, checking nothing: [`Self::finish_admitted`],
    /// [`Installed::exclusive_turn`] and [`Self::try_exclusive_installation`] check the
    /// caller under it, and waiting for idle work acts on nothing. Work holding one
    /// never asks for the installation guard after it, since only `finish_*` takes that.
    async fn unchecked_exclusive_turn(&self) -> OwnedRwLockWriteGuard<()> {
        self.owner.admission_lock().write_owned().await
    }

    /// Wait until no admitted work holds a turn.
    pub(super) async fn wait_for_admitted_work(&self) {
        drop(self.unchecked_exclusive_turn().await);
    }

    /// Release the data directory once no admitted work runs, waiting at most `within`.
    /// Work from [`Self::finish_installed`] holds no turn between its steps, so a stop
    /// that drops its turn before the removal that follows it would otherwise lose that
    /// turn to this wait, and the daemon would exit with the copy stopped, not removed.
    /// `None` when work still runs then: the record stays claimed until exit rather than
    /// failing that work halfway through.
    pub(crate) async fn close_when_idle(
        &self,
        within: std::time::Duration,
    ) -> Option<Result<(), crate::machine::RecordOwnerStopped>> {
        let _idle = tokio::time::timeout(within, async {
            let installed = self.owner.installed_work().write_owned().await;
            (installed, self.unchecked_exclusive_turn().await)
        })
        .await
        .ok()?;
        Some(self.owner.close().await)
    }

    /// The exclusive turn and the installation itself, taken only when both are free now,
    /// for an upgrade that replaces this daemon, with this Machine serving the call at the
    /// scope its caller holds under the turn.
    ///
    /// # Errors
    /// Returns [`crate::mutation::Error::Busy`] when admitted work holds a turn,
    /// [`Error::ManagementRevoked`] for a revoked management client, and the gate's
    /// refusal when an installation or mutation is active.
    pub(super) fn try_exclusive_installation(&self) -> Result<ExclusiveInstallation, Error> {
        let turn = self
            .owner
            .admission_lock()
            .try_write_owned()
            .map_err(|_| crate::mutation::Error::Busy)?;
        let local = self.clone().checked_under(&turn)?;
        let installation = self.owner.mutation_gate().try_installation()?;
        Ok(ExclusiveInstallation {
            local,
            turn,
            installation,
        })
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

/// The exclusive turn and the installation, which an upgrade holds until the worker it
/// launches takes the installation over, and this Machine serving the call at the scope
/// its caller holds under the turn.
pub(super) struct ExclusiveInstallation {
    pub(super) local: Checked,
    pub(super) turn: OwnedRwLockWriteGuard<()>,
    pub(super) installation: crate::mutation::InstallationGuard,
}
