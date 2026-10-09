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
use crate::machine::LocalMachineRecord;

const UNIT_NAME: &str = "ployz-dns.service";
const HOST_UNIT_NAME: &str = "ployz.service";
const UNIT_DIR: &str = "/run/systemd/system";
const PROBE_BOUND: Duration = Duration::from_secs(2);
const SYSTEMCTL_BOUND: Duration = Duration::from_secs(5);
const STOP_BOUND: Duration = Duration::from_secs(5);

/// Who starts and restarts `ployzd dns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsSupervision {
    /// The daemon writes `ployz-dns.service` and drives it with `systemctl`.
    Systemd,
    /// Something else runs `ployzd dns`; the daemon only publishes the spec.
    External,
}

impl DnsSupervision {
    /// Systemd only when this process runs as `ployz.service`. `NOTIFY_SOCKET`
    /// and `INVOCATION_ID` are inherited by anything a unit starts, tests
    /// included, so the cgroup decides.
    #[must_use]
    pub fn detect() -> Self {
        let cgroup = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        if runs_as_host_unit(&cgroup) {
            Self::Systemd
        } else {
            Self::External
        }
    }
}

fn runs_as_host_unit(cgroup: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|line| line.rsplit('/').next())
        .any(|unit| unit.trim_end() == HOST_UNIT_NAME)
}

#[derive(Clone)]
pub struct DnsService {
    inner: Arc<Inner>,
}

struct Inner {
    supervision: DnsSupervision,
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
        supervision: DnsSupervision,
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
        if self.inner.supervision == DnsSupervision::Systemd {
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
        if !resetting && !self.downgrade_pending().await {
            return;
        }
        if let Err(error) = tokio::time::timeout(STOP_BOUND, self.converge(None)).await {
            eprintln!("Internal DNS did not retire within {STOP_BOUND:?}: {error}");
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
                if self.inner.supervision == DnsSupervision::Systemd {
                    if fs::read_to_string(&self.inner.unit_file).ok().as_deref()
                        != Some(&self.inner.unit_text)
                    {
                        fs::write(&self.inner.unit_file, &self.inner.unit_text)?;
                        systemctl(&["daemon-reload"]).await;
                    }
                    systemctl(&["start", "--no-block", UNIT_NAME]).await;
                }
            }
            None => {
                if self.inner.supervision == DnsSupervision::Systemd {
                    systemctl(&["stop", "--no-block", UNIT_NAME]).await;
                    match fs::remove_file(&self.inner.unit_file) {
                        Ok(()) => {
                            systemctl(&["daemon-reload"]).await;
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                self.inner.spec_file.publish(None)?;
            }
        }
        Ok(())
    }

    async fn move_onto_own_binary(&self) {
        let Some(output) = systemctl(&["show", "-p", "MainPID", "--value", UNIT_NAME]).await else {
            return;
        };
        let pid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pid.is_empty() || pid == "0" {
            return;
        }
        let theirs = identity_of(&PathBuf::from(format!("/proc/{pid}/exe"))).ok();
        if restart_needed(theirs, self.inner.own_exe.identity()) {
            systemctl(&["restart", "--no-block", UNIT_NAME]).await;
        }
    }
}

pub(crate) fn restart_needed(theirs: Option<(u64, u64)>, ours: Option<(u64, u64)>) -> bool {
    matches!((theirs, ours), (Some(theirs), Some(ours)) if theirs != ours)
}

async fn systemctl(args: &[&str]) -> Option<std::process::Output> {
    let run = Command::new("systemctl")
        .args(args)
        .env_remove("NOTIFY_SOCKET")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(SYSTEMCTL_BOUND, run).await {
        Ok(Ok(output)) if output.status.success() => Some(output),
        Ok(Ok(output)) => {
            eprintln!(
                "systemctl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
            None
        }
        Ok(Err(error)) => {
            eprintln!("systemctl {} could not run: {error}", args.join(" "));
            None
        }
        Err(_) => {
            eprintln!(
                "systemctl {} did not finish within {SYSTEMCTL_BOUND:?}",
                args.join(" ")
            );
            None
        }
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
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("ployzd");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
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

    #[test]
    fn systemd_supervision_means_running_as_the_host_unit() {
        assert!(runs_as_host_unit("0::/system.slice/ployz.service\n"));
        assert!(!runs_as_host_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/t3code.service\n"
        ));
        assert!(!runs_as_host_unit("0::/system.slice/ployz-dns.service\n"));
        assert!(!runs_as_host_unit(""));
    }

    #[test]
    fn restart_needed_only_for_other_software() {
        assert!(restart_needed(Some((1, 2)), Some((1, 3))));
        assert!(!restart_needed(Some((1, 2)), Some((1, 2))));
        assert!(!restart_needed(None, Some((1, 2))));
        assert!(!restart_needed(Some((1, 2)), None));
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
