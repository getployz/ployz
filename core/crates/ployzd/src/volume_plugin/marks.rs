//! The `ployz:*` ZFS user properties a Volume's datasets carry, and what each combination
//! of them means.
//!
//! The names and the order in which each transition sets and inherits them are on disk on
//! live Machines: a daemon that stops between two `zfs` calls leaves the marks of the first,
//! and the next daemon reads them. Every transition below is a list run in order, so a stop
//! between any two entries leaves a state the readers keep rather than destroy:
//!
//! - A mark that says another step made the copy goes before the mark that says this step
//!   did. [`Marks::received`] drops the cut, epoch, promote and seed marks first and records
//!   `ployz:pulled` last, so a stop between leaves a slot with neither, which a departure
//!   keeps ([`Marks::holds_alone`], `written_here`).
//! - A copy gains the marks that make it a root before it is renamed into place. A landing
//!   seed ([`Marks::landed`]) and a promoted slot ([`Marks::promote_slot`]) carry
//!   `ployz:promote` before the rename, so a root never shows to Docker before its labels.
//! - A stop before a rename leaves a slot carrying marks that read as this Machine's own
//!   copy, which a departure and a seed both keep.
//!
//! Each read is a fresh `zfs get` at the point a decision is made, because every transition
//! re-reads between its own steps and a value read earlier goes stale across them. `get`
//! answers the effective value, inherited ones included; [`Marks::pulled_local`] answers a
//! value set on the dataset itself, since an inherited `ployz:pulled` proves nothing about
//! where the copy came from.

use std::fmt;

use ployz_core::{SlotRole, SnapshotName, WriterEpoch};

use super::{PluginError, Result, VolumeStorage};

/// A ZFS property the plugin reads or writes on a Volume's dataset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Mark {
    /// Set on a root renamed into place until Docker's Create registers it with its labels.
    /// Get and List hide a marked root, so Docker never records it without them.
    Promote,
    /// Set on a root the cut renames into its slot, so the rename carries it to the slot's
    /// `fs`. It means Docker on this Machine still holds a record for the Volume, and plugin
    /// Get keeps answering for it until a Writer want makes the root writable again.
    Cut,
    /// On a root: the SetVolumeCopy run of writes it belongs to. Missing reads as 0.
    Epoch,
    /// On the root a seed lands as: the guid of the restore point it holds. Landing sets it
    /// on the slot just before the rename, so a completed copy that has not landed carries
    /// none.
    Seed,
    /// On a slot's `fs`: the guid of the snapshot a mirror pull received, set once the
    /// receive exits having made a snapshot newer than the slot held. That snapshot came
    /// from a writer elsewhere, so while it is still the newest the departure may destroy
    /// the slot. Read only where set locally: a value inherited from a parent proves
    /// nothing.
    Pulled,
    /// On a slot parent: the target of the SetVolumeCopy pull in flight, from its start
    /// until its prune is done.
    Pull,
    /// On a slot parent: what this Machine holds the slot for, `mirror` once a pull starts
    /// and `old_copy` once a cut or a departure parks a root in it. Unset reads as a Mirror.
    Role,
    /// Set by ZFS on a dataset whose `receive -s` was interrupted; `-` otherwise. ZFS's own
    /// property, only read here.
    ResumeToken,
}

impl Mark {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Promote => "ployz:promote",
            Self::Cut => "ployz:cut",
            Self::Epoch => "ployz:epoch",
            Self::Seed => "ployz:seed",
            Self::Pulled => "ployz:pulled",
            Self::Pull => "ployz:pull",
            Self::Role => "ployz:role",
            Self::ResumeToken => "receive_resume_token",
        }
    }
}

impl fmt::Display for Mark {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One step of a transition: `zfs set <mark>=<value>` or `zfs inherit <mark>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Write {
    Set(Mark, String),
    Inherit(Mark),
}

impl Write {
    fn on(mark: Mark) -> Self {
        Self::Set(mark, "on".to_owned())
    }

    fn argv<'dataset>(&'dataset self, dataset: &'dataset str) -> [String; 3] {
        match self {
            Self::Set(mark, value) => [
                "set".to_owned(),
                format!("{mark}={value}"),
                dataset.to_owned(),
            ],
            Self::Inherit(mark) => [
                "inherit".to_owned(),
                mark.name().to_owned(),
                dataset.to_owned(),
            ],
        }
    }
}

/// The marks of one dataset. Each method reads or writes ZFS afresh.
pub(super) struct Marks<'storage> {
    storage: &'storage VolumeStorage,
    dataset: &'storage str,
}

impl VolumeStorage {
    pub(super) fn marks<'storage>(&'storage self, dataset: &'storage str) -> Marks<'storage> {
        Marks {
            storage: self,
            dataset,
        }
    }
}

impl Marks<'_> {
    /// The effective value of `mark`, or `None` when ZFS reports it unset (`-`).
    pub(super) async fn get(&self, mark: Mark) -> Result<Option<String>> {
        self.storage.property(self.dataset, mark.name()).await
    }

    pub(super) async fn is_set(&self, mark: Mark) -> Result<bool> {
        Ok(self.get(mark).await?.is_some())
    }

    /// Whether the dataset is a root Docker has not registered yet: `ployz:promote` is set.
    pub(super) async fn unregistered(&self) -> Result<bool> {
        self.is_set(Mark::Promote).await
    }

    /// Whether the dataset is a seed root Docker has not adopted: it carries both
    /// `ployz:seed` and `ployz:promote`.
    pub(super) async fn unadopted_seed(&self) -> Result<bool> {
        Ok(self.unregistered().await? && self.is_set(Mark::Seed).await?)
    }

    /// Whether a seed, promote, cut or epoch mark says the dataset holds a point this
    /// Machine may alone hold. Read in that order, stopping at the first one set.
    pub(super) async fn holds_alone(&self) -> Result<bool> {
        for mark in [Mark::Seed, Mark::Promote, Mark::Cut, Mark::Epoch] {
            if self.is_set(mark).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `ployz:pulled` as set on the dataset itself, ignoring any value it inherits.
    pub(super) async fn pulled_local(&self) -> Result<Option<String>> {
        let value = self
            .storage
            .zfs(&[
                "get",
                "-H",
                "-s",
                "local",
                "-o",
                "value",
                Mark::Pulled.name(),
                self.dataset,
            ])
            .await?;
        let value = value.trim_end_matches('\n');
        Ok((!value.is_empty() && value != "-").then(|| value.to_owned()))
    }

    /// The run of writes a root belongs to; a root SetVolumeCopy never numbered reads 0.
    pub(super) async fn epoch(&self) -> Result<WriterEpoch> {
        match self.get(Mark::Epoch).await? {
            None => Ok(WriterEpoch::default()),
            Some(value) => value.parse().map(WriterEpoch::new).map_err(|error| {
                PluginError::from(format!(
                    "{} carries an unreadable {} {value:?}: {error}",
                    self.dataset,
                    Mark::Epoch
                ))
            }),
        }
    }

    /// What this Machine recorded a slot parent as.
    pub(super) async fn role(&self) -> Result<SlotRole> {
        match self.get(Mark::Role).await? {
            None => Ok(SlotRole::default()),
            Some(value) => value.parse().map_err(|error| {
                PluginError::from(format!(
                    "{} carries an unreadable {} {value:?}: {error}",
                    self.dataset,
                    Mark::Role
                ))
            }),
        }
    }

    /// The target a slot parent's `ployz:pull` names.
    pub(super) async fn pull_target(&self) -> Result<Option<SnapshotName>> {
        let Some(target) = self.get(Mark::Pull).await? else {
            return Ok(None);
        };
        target.parse().map(Some).map_err(|error| {
            PluginError::from(format!(
                "{} carries an unreadable {} {target:?}: {error}",
                self.dataset,
                Mark::Pull
            ))
        })
    }

    /// Runs `writes` in order, stopping at the first that fails.
    pub(super) async fn apply(&self, writes: &[Write]) -> Result<()> {
        for write in writes {
            let argv = write.argv(self.dataset);
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            self.storage.zfs(&args).await?;
        }
        Ok(())
    }

    pub(super) async fn set_role(&self, role: SlotRole) -> Result<()> {
        self.apply(&[Write::Set(Mark::Role, role.as_str().to_owned())])
            .await
    }

    pub(super) async fn set_pull(&self, target: &str) -> Result<()> {
        self.apply(&[Write::Set(Mark::Pull, target.to_owned())])
            .await
    }

    pub(super) async fn clear_pull(&self) -> Result<()> {
        self.apply(&[Write::Inherit(Mark::Pull)]).await
    }

    pub(super) async fn set_cut(&self) -> Result<()> {
        self.apply(&[Write::on(Mark::Cut)]).await
    }

    pub(super) async fn set_epoch(&self, epoch: impl fmt::Display) -> Result<()> {
        self.apply(&[Write::Set(Mark::Epoch, epoch.to_string())])
            .await
    }

    /// Docker's Create registered the root, so it shows to Get and List.
    pub(super) async fn clear_promote(&self) -> Result<()> {
        self.apply(&[Write::Inherit(Mark::Promote)]).await
    }

    /// Drops the cut once the root is back and writable, if the root carries it.
    pub(super) async fn clear_cut(&self) -> Result<()> {
        if self.is_set(Mark::Cut).await? {
            self.apply(&[Write::Inherit(Mark::Cut)]).await?;
        }
        Ok(())
    }

    /// The marks of a slot's `fs` a mirror pull received `newest` into: each mark that
    /// describes a point the writer has since built on goes, then `newest` is recorded.
    pub(super) fn received(newest: impl fmt::Display) -> Vec<Write> {
        let mut writes = Self::clear_provenance();
        writes.push(Write::Set(Mark::Pulled, newest.to_string()));
        writes
    }

    /// Any cut, epoch, promote or seed mark describes a point another writer has since
    /// built on.
    pub(super) fn clear_provenance() -> Vec<Write> {
        [Mark::Cut, Mark::Epoch, Mark::Promote, Mark::Seed]
            .into_iter()
            .map(Write::Inherit)
            .collect()
    }

    /// The marks a seed lands its slot's `fs` with, just before the rename makes it the
    /// root: the slot may be a mirror that pulled this point, so its pulled mark goes first.
    pub(super) fn landed(guid: impl fmt::Display) -> Vec<Write> {
        vec![
            Write::Inherit(Mark::Pulled),
            Write::Set(Mark::Seed, guid.to_string()),
            Write::on(Mark::Promote),
        ]
    }

    /// The slot's `fs` takes a writer's marks before the rename that makes it the root:
    /// `ployz:pulled` goes, `ployz:seed` goes whenever the slot carries it, `ployz:epoch`
    /// becomes `epoch`, and `ployz:promote` is set unless the slot was cut. The reads sit
    /// between the writes, as the plugin ran them before this type, and each follows the
    /// write before it.
    pub(super) async fn promote_slot(&self, epoch: impl fmt::Display) -> Result<()> {
        self.apply(&[Write::Inherit(Mark::Pulled)]).await?;
        if self.is_set(Mark::Seed).await? {
            self.apply(&[Write::Inherit(Mark::Seed)]).await?;
        }
        self.set_epoch(epoch).await?;
        if !self.is_set(Mark::Cut).await? {
            self.apply(&[Write::on(Mark::Promote)]).await?;
        }
        Ok(())
    }
}
