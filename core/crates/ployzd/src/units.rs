//! The systemd units ployzd installs or converges, named once. Unit files on installed Machines
//! carry these names, and `scripts/uninstall.sh` removes them by name.

/// A unit ployzd writes: `install-units` puts four in `/etc/systemd/system`, the daemon writes
/// the other two under `/run/systemd/system` on every start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    /// The Machine daemon.
    Ployz,
    /// The Machine API socket that starts [`Unit::Ployz`] on connect.
    PloyzSocket,
    VolumePluginSocket,
    VolumePluginService,
    Observe,
    Dns,
    /// Puts the container network's fence back before `docker.service` at boot.
    Fence,
}

impl Unit {
    /// Units besides the daemon that an Upgrade restarts and compensation must bring back. The
    /// restored daemon moves the Observe Store onto its own binary itself.
    pub(crate) const HELPERS: [Self; 2] = [Self::VolumePluginSocket, Self::VolumePluginService];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Ployz => "ployz.service",
            Self::PloyzSocket => "ployz.socket",
            Self::VolumePluginSocket => "ployz-volume-plugin.socket",
            Self::VolumePluginService => "ployz-volume-plugin.service",
            Self::Observe => "ployz-observe.service",
            Self::Dns => "ployz-dns.service",
            Self::Fence => "ployz-fence.service",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use ployz_core::{MachineLayout, labels};

    use super::Unit;

    impl Unit {
        /// Every unit `scripts/uninstall.sh` has to stop and remove.
        const UNINSTALL_SET: [Self; 7] = [
            Self::Ployz,
            Self::PloyzSocket,
            Self::VolumePluginSocket,
            Self::VolumePluginService,
            Self::Observe,
            Self::Dns,
            Self::Fence,
        ];
    }

    /// Units other code writes and the script removes: the check-in loop that releases before
    /// Fleet Tokens were removed left on waiting hosts, and the per-attempt Upgrade workers
    /// (`installer/upgrade.rs`, `unit_name`).
    const OTHER_WRITERS: [&str; 2] = ["ployz-checkin.service", "ployz-upgrade-*.service"];

    fn uninstall_script() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/uninstall.sh");
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    #[test]
    fn unit_names_are_the_ones_installed_machines_carry() {
        assert_eq!(
            Unit::UNINSTALL_SET.map(Unit::name),
            [
                "ployz.service",
                "ployz.socket",
                "ployz-volume-plugin.socket",
                "ployz-volume-plugin.service",
                "ployz-observe.service",
                "ployz-dns.service",
                "ployz-fence.service",
            ]
        );
        assert_eq!(
            Unit::HELPERS.map(Unit::name),
            ["ployz-volume-plugin.socket", "ployz-volume-plugin.service"]
        );
    }

    #[test]
    fn the_uninstall_script_removes_every_unit_path_and_label_ployzd_installs() {
        let script = uninstall_script();
        let missing = |what: &str, wanted: &str| {
            assert!(
                script.contains(wanted),
                "scripts/uninstall.sh never mentions {what} {wanted:?}"
            );
        };
        for unit in Unit::UNINSTALL_SET {
            missing("the unit", unit.name());
        }
        for unit in OTHER_WRITERS {
            missing("the unit", unit);
        }

        let layout = MachineLayout::production();
        // The script spells each as the default of the variable it removes it through.
        for (what, path) in [
            ("the data directory", layout.data_dir().to_path_buf()),
            ("the run directory", layout.run_dir().to_path_buf()),
            ("the check-in directory", layout.checkin_dir()),
            ("the Fleet Token file", layout.fleet_token()),
        ] {
            missing(what, &format!(":-{}}}", path.display()));
        }
        missing(
            "the filter for managed containers",
            &format!("label={}", labels::MANAGED),
        );
    }
}
