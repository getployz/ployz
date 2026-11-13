//! What keeps Sealed values safe while a Machine is partway through the upgrade that
//! brings them. Delete once no supported release runs a Log Store that writes a
//! Container's env to disk.

use std::sync::Arc;

use crate::{
    runtime_unit::{
        UnitSupervision,
        process_identity::{Identity, InstalledExe, UnitProcess},
    },
    units::Unit,
};

#[derive(Clone, Default)]
pub(crate) enum LogStore {
    #[default]
    Unsupervised,
    /// `ployz-observe.service` runs it, and should run this daemon's binary. `None`
    /// when that binary could not be identified, so no Sealed value opens.
    Systemd(Option<Arc<InstalledExe>>),
    #[cfg(test)]
    Upgrading,
}

impl LogStore {
    pub(crate) fn of(units: UnitSupervision) -> Self {
        match units {
            UnitSupervision::Systemd => Self::Systemd(InstalledExe::current().ok().map(Arc::new)),
            UnitSupervision::External => Self::Unsupervised,
        }
    }

    pub(crate) async fn admit_sealed(&self) -> Result<(), LogStoreUpgrading> {
        let upgrading = match self {
            Self::Unsupervised => false,
            Self::Systemd(None) => true,
            Self::Systemd(Some(own)) => !runs_own_software(own).await,
            #[cfg(test)]
            Self::Upgrading => true,
        };
        if upgrading {
            Err(LogStoreUpgrading)
        } else {
            Ok(())
        }
    }
}

async fn runs_own_software(own: &InstalledExe) -> bool {
    same_software(UnitProcess::of(Unit::Observe.name()).await, own.identity())
}

/// Only proof of the same binary admits: a failed `systemctl` or an unreadable
/// process counts as old software. No process means nothing runs to write env.
fn same_software(process: UnitProcess, ours: Option<Identity>) -> bool {
    match process {
        UnitProcess::NoProcess => true,
        UnitProcess::Unknown => false,
        UnitProcess::Running(theirs) => ours == Some(theirs),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the Log Store still runs the software from before an upgrade")]
pub struct LogStoreUpgrading;

#[cfg(test)]
mod tests {
    use super::{UnitProcess, same_software};

    #[test]
    fn only_proof_of_the_same_binary_admits_a_sealed_value() {
        for (process, ours, admits) in [
            (UnitProcess::Running((1, 2)), Some((1, 2)), true),
            (UnitProcess::NoProcess, Some((1, 2)), true),
            (UnitProcess::NoProcess, None, true),
            (UnitProcess::Running((1, 2)), Some((1, 3)), false),
            (UnitProcess::Running((1, 2)), None, false),
            (UnitProcess::Unknown, Some((1, 2)), false),
        ] {
            assert_eq!(same_software(process, ours), admits, "{process:?} {ours:?}");
        }
    }
}
