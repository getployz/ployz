//! What keeps Sealed values safe while a Machine is partway through the upgrade that
//! brings them. Delete once no supported release runs a Log Store that writes a
//! Container's env to disk.

use std::sync::Arc;

use crate::{dns::InstalledExe, observe, runtime_unit, runtime_unit::UnitSupervision};

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
            Self::Systemd(Some(own)) => {
                runtime_unit::runs_other_software(observe::unit::UNIT_NAME, own).await
            }
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

#[derive(Debug, thiserror::Error)]
#[error("the Log Store still runs the software from before an upgrade")]
pub struct LogStoreUpgrading;
