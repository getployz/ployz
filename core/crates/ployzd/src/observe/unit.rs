//! `ployz-observe.service`, converged by the daemon on every start like
//! Internal DNS, so the Observe Store runs this daemon's binary whatever the
//! installer that put the binary in place knew about it.

use std::{io, path::Path};

use crate::{
    dns::InstalledExe,
    runtime_unit::{self, UNIT_DIR, systemctl},
};

const UNIT_NAME: &str = "ployz-observe.service";

/// Write the runtime unit and start the Observe Store, restarting it when the
/// unit changed or it runs other software. A legacy unit in `/etc` shadows the runtime one until
/// the next install removes it; both run the same binary path.
///
/// # Errors
///
/// Returns an error when the daemon's own binary cannot be identified, the
/// unit cannot be installed, or systemd refuses to start it.
pub(crate) async fn converge(data_dir: &Path, socket: &Path) -> io::Result<()> {
    converge_in(
        Path::new(UNIT_DIR),
        &InstalledExe::current()?,
        data_dir,
        socket,
    )
    .await
}

async fn converge_in(
    unit_dir: &Path,
    exe: &InstalledExe,
    data_dir: &Path,
    socket: &Path,
) -> io::Result<()> {
    let rewritten = runtime_unit::install(
        &unit_dir.join(UNIT_NAME),
        &unit_text(exe.path(), data_dir, socket),
    )
    .await?;
    let verb = if rewritten || runtime_unit::runs_other_software(UNIT_NAME, exe).await {
        "restart"
    } else {
        "start"
    };
    match systemctl(&[verb, "--no-block", UNIT_NAME]).await {
        Some(_) => Ok(()),
        None => Err(io::Error::other(format!(
            "systemctl {verb} {UNIT_NAME} failed"
        ))),
    }
}

/// The harvester runs as root to link Docker's root-only log files, so its
/// cgroup limits keep a runaway harvester from starving the Machine.
fn unit_text(exe: &Path, data_dir: &Path, socket: &Path) -> String {
    format!(
        "# Written by ployzd. Removed by uninstall and reboot.
[Unit]
Description=Ployz Log Store
After=docker.service
Wants=docker.service

[Service]
Type=simple
ExecStart={exe} --data-dir {data_dir} --socket {socket} observe
EnvironmentFile=-/etc/default/ployz
Restart=always
RestartSec=2
CPUQuota=10%
MemoryMax=256M
IOWeight=10
NoNewPrivileges=true
ProtectSystem=full
ProtectControlGroups=true
ProtectHome=read-only
ProtectKernelTunables=true
PrivateTmp=true
RestrictAddressFamilies=AF_UNIX
RestrictNamespaces=true
",
        exe = exe.display(),
        data_dir = data_dir.display(),
        socket = socket.display(),
    )
}

#[cfg(test)]
mod tests {
    use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

    use super::*;

    const CASE: &str = "PLOYZ_OBSERVE_UNIT_CASE";
    const ROOT: &str = "PLOYZ_OBSERVE_UNIT_ROOT";

    /// Logs every call; `show` answers the main PID in `main-pid`, or 0 when nothing runs.
    /// `start` and `restart` fail when `fail-start` exists.
    const SYSTEMCTL: &str = r#"echo "$*" >> "$PLOYZ_OBSERVE_UNIT_ROOT/systemctl.log"
if [ -f "$PLOYZ_OBSERVE_UNIT_ROOT/fail-start" ] && { [ "$1" = start ] || [ "$1" = restart ]; }; then exit 1; fi
if [ "$1" = show ]; then
  pid=0
  if [ -f "$PLOYZ_OBSERVE_UNIT_ROOT/main-pid" ]; then read -r pid < "$PLOYZ_OBSERVE_UNIT_ROOT/main-pid"; fi
  echo "$pid"
fi"#;

    fn write_script(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn log(root: &Path) -> Vec<String> {
        fs::read_to_string(root.join("systemctl.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.starts_with("show "))
            .map(str::to_owned)
            .collect()
    }

    /// The daemon on a Machine whose install never wrote the observe unit, as a Cloud upgrade
    /// from 0.2.0 leaves it, and on one whose observer still runs the replaced binary.
    #[test]
    fn converge_writes_starts_and_moves_the_observe_unit() {
        if let Ok(case) = env::var(CASE) {
            let root = PathBuf::from(env::var_os(ROOT).unwrap());
            let exe = InstalledExe::current().unwrap();
            let converge = || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(converge_in(
                        &root.join("units"),
                        &exe,
                        Path::new("/var/lib/ployz"),
                        Path::new("/run/ployz/ployz.sock"),
                    ))
            };
            if case == "start-fails" {
                assert!(converge().is_err(), "a refused start is reported");
            } else {
                converge().unwrap();
            }
            if case == "current" {
                // A daemon restart on the same software finds its observer already running it.
                fs::remove_file(root.join("systemctl.log")).unwrap();
                fs::write(root.join("main-pid"), std::process::id().to_string()).unwrap();
                converge().unwrap();
            }
            fs::write(root.join("child-completed"), case).unwrap();
            return;
        }

        for case in [
            "absent",
            "stale-definition",
            "other-software",
            "current",
            "start-fails",
        ] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path();
            fs::create_dir_all(root.join("commands")).unwrap();
            fs::create_dir_all(root.join("units")).unwrap();
            write_script(&root.join("commands/systemctl"), SYSTEMCTL);
            if case == "stale-definition" {
                // An observer on this binary still running a unit written with other settings.
                fs::write(root.join("units/ployz-observe.service"), "[Service]\n").unwrap();
                fs::write(root.join("main-pid"), std::process::id().to_string()).unwrap();
            }
            if case == "start-fails" {
                fs::write(root.join("fail-start"), "").unwrap();
            }
            let mut stale = None;
            if case == "other-software" {
                // An observer started from another file, as one is after a binary swap.
                let sleeper = Command::new("sleep").arg("30").spawn().unwrap();
                fs::write(root.join("main-pid"), sleeper.id().to_string()).unwrap();
                stale = Some(sleeper);
            }
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "observe::unit::tests::converge_writes_starts_and_moves_the_observe_unit",
                    "--nocapture",
                ])
                .env("PATH", root.join("commands"))
                .env(CASE, case)
                .env(ROOT, root)
                .output()
                .unwrap();
            if let Some(mut sleeper) = stale {
                sleeper.kill().unwrap();
                sleeper.wait().unwrap();
            }
            assert!(
                output.status.success(),
                "{case}: child failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(root.join("child-completed")).unwrap(),
                case
            );

            let unit = fs::read_to_string(root.join("units/ployz-observe.service"))
                .unwrap_or_else(|error| panic!("{case}: no runtime observe unit: {error}"));
            let exe = InstalledExe::current().unwrap();
            assert!(
                unit.contains(&format!(
                    "\nExecStart={} --data-dir /var/lib/ployz --socket /run/ployz/ployz.sock observe\n",
                    exe.path().display()
                )),
                "{case}: unit does not run this daemon's binary:\n{unit}"
            );
            assert!(
                !unit.contains("[Install]"),
                "{case}: runtime unit is enabled"
            );
            let expected: &[&str] = match case {
                "current" => &["start --no-block ployz-observe.service"],
                _ => &["daemon-reload", "restart --no-block ployz-observe.service"],
            };
            assert_eq!(log(root), expected, "{case}");
        }
    }
}
