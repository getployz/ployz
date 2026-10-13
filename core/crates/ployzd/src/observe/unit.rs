//! `ployz-observe.service`, converged by the daemon on every start.

use std::{io, path::Path};

use crate::dns::InstalledExe;

/// Converge `ployz-observe.service` in `unit_dir` onto `exe`.
///
/// # Errors
///
/// Returns an error when the unit cannot be written.
pub(crate) async fn converge_in(
    _unit_dir: &Path,
    _exe: &InstalledExe,
    _data_dir: &Path,
    _socket: &Path,
) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

    use super::*;

    const CASE: &str = "PLOYZ_OBSERVE_UNIT_CASE";
    const ROOT: &str = "PLOYZ_OBSERVE_UNIT_ROOT";

    /// Logs every call; `show` answers the main PID in `main-pid`, or 0 when nothing runs.
    const SYSTEMCTL: &str = r#"echo "$*" >> "$PLOYZ_OBSERVE_UNIT_ROOT/systemctl.log"
if [ "$1" = show ]; then
  if [ -f "$PLOYZ_OBSERVE_UNIT_ROOT/main-pid" ]; then cat "$PLOYZ_OBSERVE_UNIT_ROOT/main-pid"; else echo 0; fi
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
    fn daemon_start_converges_the_observe_unit() {
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
                    .unwrap();
            };
            converge();
            if case == "current" {
                // A daemon restart on the same software finds its observer already running it.
                fs::remove_file(root.join("systemctl.log")).unwrap();
                fs::write(root.join("main-pid"), std::process::id().to_string()).unwrap();
                converge();
            }
            fs::write(root.join("child-completed"), case).unwrap();
            return;
        }

        for case in ["absent", "other-software", "current"] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path();
            fs::create_dir_all(root.join("commands")).unwrap();
            fs::create_dir_all(root.join("units")).unwrap();
            write_script(&root.join("commands/systemctl"), SYSTEMCTL);
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
                    "observe::unit::tests::daemon_start_converges_the_observe_unit",
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
            assert!(!unit.contains("[Install]"), "{case}: runtime unit is enabled");
            let expected: &[&str] = match case {
                "absent" => &["daemon-reload", "start --no-block ployz-observe.service"],
                "other-software" => &["daemon-reload", "restart --no-block ployz-observe.service"],
                _ => &["start --no-block ployz-observe.service"],
            };
            assert_eq!(log(root), expected, "{case}");
        }
    }
}
