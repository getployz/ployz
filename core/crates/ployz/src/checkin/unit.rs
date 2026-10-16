//! The two systemd units a waiting host runs: the check-in loop and preparation.

use std::path::Path;

pub(crate) const CHECKIN: &str = "ployz-checkin.service";
pub(crate) const PREPARE: &str = "ployz-prepare.service";

pub(crate) fn checkin(ployz: &Path, token_file: &Path, cloud_url: &str) -> String {
    format!(
        "[Unit]
Description=Ployz check-in: wait for approval in Ployz Cloud
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=PLOYZ_CLOUD_URL={cloud_url}
ExecStart={ployz} server checkin --token-file {token_file}
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
",
        ployz = ployz.display(),
        token_file = token_file.display(),
    )
}

pub(crate) fn prepare(ployzd: &Path, skip_zfs: bool) -> String {
    let skip = if skip_zfs { " --skip-zfs" } else { "" };
    format!(
        "[Unit]
Description=Ployz host preparation: install Docker and build ZFS ahead of enroll
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart={ployzd} install --prepare{skip}
TimeoutStartSec=infinity
",
        ployzd = ployzd.display(),
    )
}
