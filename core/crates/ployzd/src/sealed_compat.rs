//! What keeps Sealed values safe while a Machine is partway through the upgrade that
//! brings them. Delete once no supported release runs a Log Store that writes a
//! Container's env to disk.

use std::{path::PathBuf, sync::Arc};

use crate::{
    dns::{InstalledExe, identity_of},
    observe, runtime_unit,
    runtime_unit::UnitSupervision,
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
    let main_pid =
        runtime_unit::systemctl(&["show", "-p", "MainPID", "--value", observe::unit::UNIT_NAME])
            .await
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let theirs = main_pid
        .as_deref()
        .and_then(|pid| identity_of(&PathBuf::from(format!("/proc/{pid}/exe"))).ok());
    same_software(main_pid.as_deref(), theirs, own.identity())
}

/// Only proof of the same binary admits: a failed `systemctl` or an unreadable
/// process counts as old software. `MainPID` 0 means nothing runs to write env.
fn same_software(
    main_pid: Option<&str>,
    theirs: Option<(u64, u64)>,
    ours: Option<(u64, u64)>,
) -> bool {
    match main_pid {
        None => false,
        Some("0") => true,
        Some(_) => matches!((theirs, ours), (Some(theirs), Some(ours)) if theirs == ours),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the Log Store still runs the software from before an upgrade")]
pub struct LogStoreUpgrading;

#[cfg(test)]
mod tests {
    use super::same_software;

    #[test]
    fn only_proof_of_the_same_binary_admits_a_sealed_value() {
        for (main_pid, theirs, ours, admits) in [
            (Some("42"), Some((1, 2)), Some((1, 2)), true),
            (Some("0"), None, Some((1, 2)), true),
            (Some("42"), Some((1, 2)), Some((1, 3)), false),
            (Some("42"), None, Some((1, 2)), false),
            (Some("42"), Some((1, 2)), None, false),
            (Some(""), None, Some((1, 2)), false),
            (None, None, Some((1, 2)), false),
        ] {
            assert_eq!(
                same_software(main_pid, theirs, ours),
                admits,
                "{main_pid:?} {theirs:?} {ours:?}"
            );
        }
    }
}
