//! The two systemd units a waiting host runs: the check-in loop and preparation.

use std::path::Path;

pub(crate) const CHECKIN: &str = "ployz-checkin.service";
pub(crate) const PREPARE: &str = "ployz-prepare.service";

/// The loop runs the ployzd `--install` chose, not whatever PATH holds when it starts. It
/// keeps `release_mirrors`, the overrides `--install` ran with, since enrolling installs the
/// release again.
pub(crate) fn checkin(
    ployz: &Path,
    ployzd: &Path,
    token_file: &Path,
    cloud_url: &str,
    release_mirrors: &[(&str, String)],
) -> Result<String, String> {
    let ployz = exec_arg(&ployz.display().to_string())?;
    let ployzd = exec_arg(&ployzd.display().to_string())?;
    let token_file = exec_arg(&token_file.display().to_string())?;
    let cloud_url = quotable(cloud_url)?.replace('%', "%%");
    let mut mirrors = String::new();
    for (name, url) in release_mirrors {
        let url = quotable(url)?.replace('%', "%%");
        mirrors.push_str(&format!("Environment=\"{name}={url}\"\n"));
    }
    Ok(format!(
        "[Unit]
Description=Ployz check-in: wait for approval in Ployz Cloud
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=600
StartLimitBurst=5

[Service]
Type=simple
Environment=\"PLOYZ_CLOUD_URL={cloud_url}\"
{mirrors}ExecStart=\"{ployz}\" server checkin --token-file \"{token_file}\" --ployzd \"{ployzd}\"
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
"
    ))
}

/// A value that can sit inside a double-quoted unit setting without ending it.
fn quotable(value: &str) -> Result<&str, String> {
    if value.contains(['\n', '\r', '\0', '"', '\\']) {
        Err(format!("{value:?} can't go in a systemd unit."))
    } else {
        Ok(value)
    }
}

fn exec_arg(value: &str) -> Result<String, String> {
    Ok(quotable(value)?.replace('%', "%%").replace('$', "$$"))
}

pub(crate) fn prepare(ployzd: &Path, skip_zfs: bool) -> Result<String, String> {
    let ployzd = exec_arg(&ployzd.display().to_string())?;
    let skip = if skip_zfs { " --skip-zfs" } else { "" };
    Ok(format!(
        "[Unit]
Description=Ployz host preparation: install Docker and build ZFS ahead of enroll
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=\"{ployzd}\" install --prepare{skip}
TimeoutStartSec=infinity
"
    ))
}
