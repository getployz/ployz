use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tokio::{process::Command, sync::watch};
use tokio_util::sync::CancellationToken;

use super::{
    process::{InstalledExe, identity_of},
    spec::{DnsSpec, SpecFile},
};
use crate::{
    machine::LocalMachineRecord,
    runtime_unit::{self, UNIT_DIR, UnitSupervision, restart_needed, systemctl},
};

const UNIT_NAME: &str = "ployz-dns.service";
const PROBE_BOUND: Duration = Duration::from_secs(2);
const STOP_BOUND: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct DnsService {
    inner: Arc<Inner>,
}

struct Inner {
    supervision: UnitSupervision,
    data_dir: PathBuf,
    run_dir: PathBuf,
    upstreams: Vec<SocketAddr>,
    spec_file: SpecFile,
    unit_file: PathBuf,
    unit_text: String,
    own_exe: InstalledExe,
}

impl DnsService {
    /// Converge the DNS process onto `record` once; idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error when the daemon's own binary cannot be identified or the
    /// spec or unit cannot be written.
    pub async fn start(
        supervision: UnitSupervision,
        data_dir: &Path,
        run_dir: &Path,
        socket: &Path,
        upstreams: Vec<SocketAddr>,
        record: &LocalMachineRecord,
    ) -> io::Result<Self> {
        let own_exe = InstalledExe::current()?;
        let service = Self {
            inner: Arc::new(Inner {
                supervision,
                data_dir: data_dir.to_path_buf(),
                run_dir: run_dir.to_path_buf(),
                upstreams,
                spec_file: SpecFile::in_run_dir(run_dir),
                unit_file: Path::new(UNIT_DIR).join(UNIT_NAME),
                unit_text: unit_text(own_exe.path(), socket),
                own_exe,
            }),
        };
        service.converge(service.spec_of(record).as_ref()).await?;
        Ok(service)
    }

    /// Restart the process onto this daemon's binary once, then follow the
    /// record until shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when a spec or unit write fails.
    pub async fn run(
        &self,
        mut records: watch::Receiver<Arc<LocalMachineRecord>>,
        shutdown: CancellationToken,
    ) -> io::Result<()> {
        if self.inner.supervision == UnitSupervision::Systemd {
            self.move_onto_own_binary().await;
        }
        loop {
            tokio::select! {
                changed = records.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                () = shutdown.cancelled() => return Ok(()),
            }
            let record = records.borrow_and_update().clone();
            self.converge(self.spec_of(&record).as_ref()).await?;
        }
    }

    /// Stop-time work, bounded so it fits the daemon's stop budget. A reset
    /// removes the process; a downgrade onto a binary that cannot serve DNS
    /// retires it so that binary can bind port 53. Never fails.
    pub async fn stopping(&self, resetting: bool) {
        let retire = async {
            if resetting || self.downgrade_pending().await {
                self.converge(None).await
            } else {
                Ok(())
            }
        };
        match tokio::time::timeout(STOP_BOUND, retire).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("Internal DNS did not retire: {error}"),
            Err(error) => eprintln!("Internal DNS did not retire within {STOP_BOUND:?}: {error}"),
        }
    }

    fn spec_of(&self, record: &LocalMachineRecord) -> Option<DnsSpec> {
        DnsSpec::of(
            record,
            &self.inner.upstreams,
            &self.inner.data_dir,
            &self.inner.run_dir,
        )
    }

    async fn downgrade_pending(&self) -> bool {
        let path = self.inner.own_exe.path();
        let installed = identity_of(path).ok();
        restart_needed(installed, self.inner.own_exe.identity())
            && Successor::at(path).probe().await == Probe::CannotServe
    }

    async fn converge(&self, spec: Option<&DnsSpec>) -> io::Result<()> {
        match spec {
            Some(spec) => {
                self.inner.spec_file.publish(Some(spec))?;
                if self.inner.supervision == UnitSupervision::Systemd {
                    runtime_unit::install(&self.inner.unit_file, &self.inner.unit_text).await?;
                    systemctl(&["start", "--no-block", UNIT_NAME]).await;
                }
            }
            None => {
                if self.inner.supervision == UnitSupervision::Systemd {
                    retire_unit(&self.inner.unit_file).await?;
                }
                self.inner.spec_file.publish(None)?;
            }
        }
        Ok(())
    }

    async fn move_onto_own_binary(&self) {
        if runtime_unit::runs_other_software(UNIT_NAME, &self.inner.own_exe).await {
            systemctl(&["restart", "--no-block", UNIT_NAME]).await;
        }
    }
}

/// Stop the unit, then remove its file and reload. The file stays while the
/// stop is unfinished: systemd 255, as Ubuntu 24.04 ships it, closes a unit's
/// stored fds twice when a reload lowers its `FileDescriptorStoreMax`, as a
/// missing file does, and PID 1 aborts (systemd/systemd#35288, fixed in 257).
/// A stopped unit has released its store.
async fn retire_unit(unit_file: &Path) -> io::Result<()> {
    if systemctl(&["stop", UNIT_NAME]).await.is_none() {
        return Ok(());
    }
    match fs::remove_file(unit_file) {
        Ok(()) => {
            systemctl(&["daemon-reload"]).await;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn unit_text(exe: &Path, socket: &Path) -> String {
    format!(
        "# Written by ployzd. Removed by reset, uninstall, and reboot.
[Unit]
Description=Ployz internal DNS
StartLimitIntervalSec=0

[Service]
Type=notify
ExecStart={exe} --socket {socket} dns
EnvironmentFile=-/etc/default/ployz
Restart=always
RestartSec=1
RestartPreventExitStatus=78
SuccessExitStatus=78
FileDescriptorStoreMax=4
TimeoutStopSec=10
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ProtectControlGroups=true
ProtectKernelTunables=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_DAC_OVERRIDE
",
        exe = exe.display(),
        socket = socket.display(),
    )
}

pub(crate) struct Successor {
    exe: PathBuf,
}

impl Successor {
    pub(crate) fn at(exe: &Path) -> Self {
        Self {
            exe: exe.to_path_buf(),
        }
    }

    pub(crate) async fn probe(&self) -> Probe {
        let probe = Command::new(&self.exe)
            .args(["dns", "--probe"])
            .env_remove("NOTIFY_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        match tokio::time::timeout(PROBE_BOUND, probe).await {
            Ok(Ok(status)) if status.success() => Probe::Serves,
            Ok(Ok(_)) => Probe::CannotServe,
            Ok(Err(_)) | Err(_) => Probe::Unknown,
        }
    }
}

/// What `dns --probe` said about the binary now installed. Only `CannotServe`
/// retires the running process; `Unknown` leaves it in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    Serves,
    CannotServe,
    Unknown,
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;
    use crate::dns::tests::write_script;

    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("ployzd");
        write_script(&path, body);
        path
    }

    #[test]
    fn unit_text_keeps_sockets_and_has_no_install() {
        let text = unit_text(
            Path::new("/usr/local/bin/ployzd"),
            Path::new("/run/ployz/ployz.sock"),
        );
        assert!(
            text.contains("ExecStart=/usr/local/bin/ployzd --socket /run/ployz/ployz.sock dns\n")
        );
        assert!(text.contains("FileDescriptorStoreMax=4\n"));
        assert!(text.contains("RestartPreventExitStatus=78\n"));
        assert!(text.contains("SuccessExitStatus=78\n"));
        assert!(text.contains("Type=notify\n"));
        assert!(!text.contains("[Install]"));
    }

    const RETIRE_CASE: &str = "PLOYZ_DNS_RETIRE_CASE";
    const RETIRE_ROOT: &str = "PLOYZ_DNS_RETIRE_ROOT";

    /// Models the unit's stop as a job: `stop --no-block` returns while the
    /// unit is still stopping. A blocking `stop` returns once it stopped, or
    /// fails when `stop-fails` exists, as one that outlives its bound does. A
    /// `daemon-reload` that finds the unit file gone before the unit stopped
    /// marks `pid1-aborted`, as systemd 255 does (systemd/systemd#35288).
    const RETIRE_SYSTEMCTL: &str = r#"root="$PLOYZ_DNS_RETIRE_ROOT"
echo "$*" >> "$root/systemctl.log"
case "$1" in
  stop)
    if [ "$2" = --no-block ]; then exit 0; fi
    if [ -f "$root/stop-fails" ]; then exit 1; fi
    : > "$root/stopped" ;;
  daemon-reload)
    if [ ! -f "$root/ployz-dns.service" ] && [ ! -f "$root/stopped" ]; then : > "$root/pid1-aborted"; fi ;;
esac"#;

    /// Reset and downgrade retire Internal DNS while its process holds
    /// listeners in the unit's fd store.
    #[test]
    fn retire_reloads_systemd_only_after_the_unit_stopped() {
        if let Ok(case) = env::var(RETIRE_CASE) {
            let root = PathBuf::from(env::var_os(RETIRE_ROOT).unwrap());
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(retire_unit(&root.join(UNIT_NAME)))
                .unwrap();
            fs::write(root.join("child-completed"), case).unwrap();
            return;
        }

        for case in ["stops", "stop-fails"] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path();
            fs::create_dir_all(root.join("commands")).unwrap();
            write_script(&root.join("commands/systemctl"), RETIRE_SYSTEMCTL);
            fs::write(root.join(UNIT_NAME), "[Service]\n").unwrap();
            if case == "stop-fails" {
                fs::write(root.join("stop-fails"), "").unwrap();
            }
            let output = std::process::Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "dns::service::tests::retire_reloads_systemd_only_after_the_unit_stopped",
                    "--nocapture",
                ])
                .env("PATH", root.join("commands"))
                .env(RETIRE_CASE, case)
                .env(RETIRE_ROOT, root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: child failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(root.join("child-completed")).unwrap(),
                case
            );
            assert!(
                !root.join("pid1-aborted").exists(),
                "{case}: reloaded while the unit was still stopping"
            );
            let log = fs::read_to_string(root.join("systemctl.log")).unwrap();
            let unit_kept = root.join(UNIT_NAME).exists();
            match case {
                "stops" => {
                    assert_eq!(log, "stop ployz-dns.service\ndaemon-reload\n");
                    assert!(!unit_kept, "the stopped unit's file is removed");
                }
                _ => {
                    assert_eq!(log, "stop ployz-dns.service\n");
                    assert!(
                        unit_kept,
                        "a unit that may still be stopping keeps its file"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn successor_definite_failure_cannot_serve() {
        let dir = tempfile::tempdir().unwrap();
        let exe = script(dir.path(), "exit 2");
        assert_eq!(Successor::at(&exe).probe().await, Probe::CannotServe);
    }

    #[tokio::test]
    async fn successor_success_serves() {
        let dir = tempfile::tempdir().unwrap();
        let exe = script(
            dir.path(),
            "[ \"$1\" = dns ] && [ \"$2\" = --probe ] && exit 0; exit 1",
        );
        assert_eq!(Successor::at(&exe).probe().await, Probe::Serves);
    }

    #[tokio::test]
    async fn successor_missing_or_hung_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Successor::at(&dir.path().join("missing")).probe().await,
            Probe::Unknown
        );
        let exe = script(dir.path(), "sleep 30");
        let started = std::time::Instant::now();
        assert_eq!(Successor::at(&exe).probe().await, Probe::Unknown);
        assert!(started.elapsed() < PROBE_BOUND + Duration::from_secs(1));
    }
}
