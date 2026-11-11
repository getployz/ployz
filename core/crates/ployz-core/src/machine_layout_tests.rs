use std::path::{Path, PathBuf};

use super::*;

/// Every literal below is what an installed Machine, its units and every released CLI already use.
#[test]
fn production_layout_keeps_the_paths_installed_machines_use() {
    let layout = MachineLayout::production();
    let table: [(&str, PathBuf, &str); 14] = [
        ("run directory", layout.run_dir().into(), "/run/ployz"),
        ("data directory", layout.data_dir().into(), "/var/lib/ployz"),
        ("socket", layout.socket(), "/run/ployz/ployz.sock"),
        (
            "observe socket",
            layout.observe_socket(),
            "/run/ployz/observe.sock",
        ),
        ("fault file", layout.fault_file(), "/run/ployz/fault"),
        (
            "machine pool file",
            layout.machine_pool_file(),
            "/var/lib/ployz-machine-pool",
        ),
        (
            "device pool marker",
            layout.device_pool_marker(),
            "/var/lib/ployz-machine-pool.device",
        ),
        (
            "check-in directory",
            layout.checkin_dir(),
            "/var/lib/ployz-checkin",
        ),
        (
            "Fleet Token",
            layout.fleet_token(),
            "/etc/ployz/fleet-token",
        ),
        (
            "prepare directory",
            layout.prepare_dir(),
            "/var/lib/ployz/prepare",
        ),
        (
            "prepare state",
            layout.prepare_state(),
            "/var/lib/ployz/prepare/state.json",
        ),
        (
            "prepare lock",
            layout.prepare_lock(),
            "/var/lib/ployz/prepare/lock",
        ),
        (
            "prepare ZFS directory",
            layout.prepare_zfs_dir(),
            "/var/lib/ployz/prepare/zfs",
        ),
        ("prepare directory name", PREPARE_DIR_NAME.into(), "prepare"),
    ];
    for (what, path, today) in table {
        assert_eq!(path, Path::new(today), "{what}");
    }
}

#[test]
fn a_layout_under_other_directories_moves_every_file_with_them() {
    let layout = MachineLayout::new("/t/run", "/t/lib/ployz", "/t/etc");
    assert_eq!(layout.socket(), Path::new("/t/run/ployz.sock"));
    assert_eq!(
        layout.machine_pool_file(),
        Path::new("/t/lib/ployz-machine-pool")
    );
    assert_eq!(
        layout.device_pool_marker(),
        Path::new("/t/lib/ployz-machine-pool.device")
    );
    assert_eq!(layout.checkin_dir(), Path::new("/t/lib/ployz-checkin"));
    assert_eq!(layout.fleet_token(), Path::new("/t/etc/fleet-token"));
    assert_eq!(
        layout.prepare_state(),
        Path::new("/t/lib/ployz/prepare/state.json")
    );
}
