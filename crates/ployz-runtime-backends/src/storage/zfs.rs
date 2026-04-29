use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::spec::parse_quota_bytes;

use super::{ShellRunner, ShellStdio, ShellStreamer, TokioShellRunner};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetSpec {
    pub dataset: String,
    pub mountpoint: PathBuf,
    pub quota: String,
    pub mode: String,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    pub mountpoint: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetInspection {
    pub dataset: String,
    pub quota: String,
    pub mountpoint: PathBuf,
    pub used_bytes: u64,
    pub snapshots: Vec<SnapshotInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub name: String,
    pub guid: u64,
}

#[derive(Debug, Clone)]
pub struct ZfsDriver<R> {
    runner: R,
    zfs_root_dataset: String,
    zfs_root_mountpoint: PathBuf,
    overcommit_ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExistingDataset {
    quota: String,
    mountpoint: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathPermissions {
    mode: String,
    owner: String,
}

impl<R: ShellRunner> ZfsDriver<R> {
    pub async fn new(runner: R, zfs_root: &str, overcommit_ratio: f64) -> Result<Self> {
        if !overcommit_ratio.is_finite() || overcommit_ratio <= 0.0 {
            return Err(Error::operation(
                "zfs_driver",
                format!(
                    "overcommit_ratio must be a positive finite number, got {overcommit_ratio}"
                ),
            ));
        }
        let output = runner
            .run("zfs", &["list", "-H", "-o", "mountpoint", zfs_root])
            .await?;
        ensure_success("zfs list root", &output.stderr, output.status)?;
        let mountpoint = parse_single_field(&output.stdout, "root mountpoint")?;
        Ok(Self {
            runner,
            zfs_root_dataset: zfs_root.to_string(),
            zfs_root_mountpoint: PathBuf::from(mountpoint),
            overcommit_ratio,
        })
    }

    pub async fn ensure(&self, spec: &DatasetSpec) -> Result<MountInfo> {
        let existing = self.read_dataset(&spec.dataset).await?;
        match existing {
            Some(existing) => {
                if existing.mountpoint != spec.mountpoint {
                    return Err(Error::operation(
                        "zfs_ensure",
                        format!(
                            "dataset '{}' has mountpoint '{}', expected '{}'",
                            spec.dataset,
                            existing.mountpoint.display(),
                            spec.mountpoint.display()
                        ),
                    ));
                }
                if existing.quota != spec.quota {
                    self.update_quota(&spec.dataset, &existing.quota, &spec.quota)
                        .await?;
                }
                self.reconcile_permissions(spec).await?;
            }
            None => self.create_dataset(spec).await?,
        }
        Ok(MountInfo {
            mountpoint: spec.mountpoint.clone(),
        })
    }

    async fn reconcile_permissions(&self, spec: &DatasetSpec) -> Result<()> {
        let mountpoint = spec.mountpoint.to_string_lossy();
        let current = self.read_path_permissions(&mountpoint).await?;
        if !mode_matches(&current.mode, &spec.mode) {
            self.run_success("chmod", "chmod", &[&spec.mode, &mountpoint])
                .await?;
        }
        if current.owner != spec.owner {
            self.run_success("chown", "chown", &[&spec.owner, &mountpoint])
                .await?;
        }
        Ok(())
    }

    async fn read_path_permissions(&self, mountpoint: &str) -> Result<PathPermissions> {
        let output = self
            .runner
            .run("stat", &["-c", "%a:%u:%g", mountpoint])
            .await?;
        ensure_success("stat mountpoint", &output.stderr, output.status)?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let mut parts = raw.splitn(3, ':');
        let mode = parts
            .next()
            .ok_or_else(|| Error::operation("stat mountpoint", format!("missing mode in '{raw}'")))?
            .to_string();
        let uid = parts.next().ok_or_else(|| {
            Error::operation("stat mountpoint", format!("missing uid in '{raw}'"))
        })?;
        let gid = parts.next().ok_or_else(|| {
            Error::operation("stat mountpoint", format!("missing gid in '{raw}'"))
        })?;
        Ok(PathPermissions {
            mode,
            owner: format!("{uid}:{gid}"),
        })
    }

    pub async fn inspect_dataset(&self, dataset: &str) -> Result<DatasetInspection> {
        let existing = self.read_dataset(dataset).await?.ok_or_else(|| {
            Error::operation("zfs_inspect", format!("dataset '{dataset}' does not exist"))
        })?;
        let used_bytes = self.used_bytes(dataset).await?;
        let snapshots = self.list_snapshots(dataset).await?;
        Ok(DatasetInspection {
            dataset: dataset.to_string(),
            quota: existing.quota,
            mountpoint: existing.mountpoint,
            used_bytes,
            snapshots,
        })
    }

    pub async fn dataset_exists(&self, dataset: &str) -> Result<bool> {
        Ok(self.read_dataset(dataset).await?.is_some())
    }

    /// Idempotently create the parent dataset of `dataset` (and any ancestors up
    /// to the root) so that a subsequent `zfs recv` can land into it. The root
    /// dataset itself is assumed to exist; we only create intermediate
    /// namespaces.
    pub async fn ensure_parent_dataset(&self, dataset: &str) -> Result<()> {
        let Some((parent, _)) = dataset.rsplit_once('/') else {
            return Err(Error::operation(
                "zfs_ensure_parent",
                format!("dataset '{dataset}' has no parent"),
            ));
        };
        if parent.is_empty() || parent == self.zfs_root_dataset {
            return Ok(());
        }
        if self.dataset_exists(parent).await? {
            return Ok(());
        }
        self.run_success("zfs create parent", "zfs", &["create", "-p", parent])
            .await
    }

    pub async fn destroy_snapshot(&self, dataset: &str, snapshot: &str) -> Result<()> {
        let full = snapshot_name(dataset, snapshot);
        let output = self.runner.run("zfs", &["destroy", &full]).await?;
        if output.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist") || stderr.contains("dataset does not exist") {
            return Ok(());
        }
        Err(Error::operation(
            "zfs destroy snapshot",
            stderr.trim().to_string(),
        ))
    }

    pub async fn create_snapshot(&self, dataset: &str, snapshot: &str) -> Result<SnapshotInfo> {
        if self.snapshot_exists(dataset, snapshot).await? {
            return Ok(SnapshotInfo {
                name: snapshot.to_string(),
                guid: self.snapshot_guid(dataset, snapshot).await?,
            });
        }
        let full = snapshot_name(dataset, snapshot);
        self.run_success("zfs snapshot", "zfs", &["snapshot", &full])
            .await?;
        Ok(SnapshotInfo {
            name: snapshot.to_string(),
            guid: self.snapshot_guid(dataset, snapshot).await?,
        })
    }

    pub async fn snapshot_exists(&self, dataset: &str, snapshot: &str) -> Result<bool> {
        let full = snapshot_name(dataset, snapshot);
        let output = self
            .runner
            .run(
                "zfs",
                &["list", "-H", "-t", "snapshot", "-o", "name", &full],
            )
            .await?;
        if output.status == 0 {
            return Ok(true);
        }
        if zfs_reports_not_found(&output.stderr) {
            return Ok(false);
        }
        Err(Error::operation(
            "zfs list snapshot",
            command_failure_message(&output.stderr, output.status),
        ))
    }

    pub async fn snapshot_guid(&self, dataset: &str, snapshot: &str) -> Result<u64> {
        let full = snapshot_name(dataset, snapshot);
        let output = self
            .runner
            .run("zfs", &["get", "-Hp", "-o", "value", "guid", &full])
            .await?;
        ensure_success("zfs get snapshot guid", &output.stderr, output.status)?;
        let value = parse_single_field(&output.stdout, "snapshot guid")?;
        value
            .parse::<u64>()
            .map_err(|err| Error::operation("zfs_parse", format!("parse snapshot guid: {err}")))
    }

    pub async fn list_snapshots(&self, dataset: &str) -> Result<Vec<SnapshotInfo>> {
        let output = self
            .runner
            .run(
                "zfs",
                &[
                    "list",
                    "-Hp",
                    "-t",
                    "snapshot",
                    "-o",
                    "name,guid",
                    "-r",
                    dataset,
                ],
            )
            .await?;
        if output.status != 0 {
            return Ok(Vec::new());
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|err| Error::operation("zfs_parse", err.to_string()))?;
        let mut snapshots = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parts = trimmed.split('\t').collect::<Vec<_>>();
            let [name, guid] = parts.as_slice() else {
                return Err(Error::operation(
                    "zfs_parse",
                    format!("expected snapshot name and guid for line '{trimmed}'"),
                ));
            };
            let Some((listed_dataset, snap)) = name.split_once('@') else {
                continue;
            };
            if listed_dataset != dataset {
                continue;
            }
            snapshots.push(SnapshotInfo {
                name: snap.to_string(),
                guid: guid.parse::<u64>().map_err(|err| {
                    Error::operation("zfs_parse", format!("parse snapshot guid: {err}"))
                })?,
            });
        }
        Ok(snapshots)
    }

    #[must_use]
    pub fn root_dataset(&self) -> &str {
        &self.zfs_root_dataset
    }

    #[must_use]
    pub fn root_mountpoint(&self) -> &Path {
        &self.zfs_root_mountpoint
    }

    async fn read_dataset(&self, dataset: &str) -> Result<Option<ExistingDataset>> {
        let output = self
            .runner
            .run(
                "zfs",
                &["list", "-Hp", "-o", "name,quota,mountpoint", dataset],
            )
            .await?;
        if output.status != 0 {
            if zfs_reports_not_found(&output.stderr) {
                return Ok(None);
            }
            return Err(Error::operation(
                "zfs list dataset",
                command_failure_message(&output.stderr, output.status),
            ));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|err| Error::operation("zfs_parse", err.to_string()))?;
        let line = text.trim();
        let parts = line.split('\t').collect::<Vec<_>>();
        let [name, quota, mountpoint] = parts.as_slice() else {
            return Err(Error::operation(
                "zfs_parse",
                format!("expected name, quota, mountpoint for dataset '{dataset}'"),
            ));
        };
        if *name != dataset {
            return Err(Error::operation(
                "zfs_parse",
                format!("zfs listed dataset '{name}', expected '{dataset}'"),
            ));
        }
        Ok(Some(ExistingDataset {
            quota: normalize_quota(quota),
            mountpoint: PathBuf::from(mountpoint),
        }))
    }

    async fn create_dataset(&self, spec: &DatasetSpec) -> Result<()> {
        let requested_bytes =
            parse_quota_bytes(&spec.quota).map_err(|err| Error::operation("zfs_quota", err))?;
        self.check_overcommit(&spec.dataset, requested_bytes)
            .await?;

        let mountpoint = spec.mountpoint.to_string_lossy();
        let quota = format!("quota={}", spec.quota);
        let mountpoint_opt = format!("mountpoint={mountpoint}");
        let output = self
            .runner
            .run(
                "zfs",
                &[
                    "create",
                    "-p",
                    "-o",
                    &quota,
                    "-o",
                    &mountpoint_opt,
                    "-o",
                    "compression=lz4",
                    &spec.dataset,
                ],
            )
            .await?;
        ensure_success("zfs create", &output.stderr, output.status)?;
        self.run_success("chmod", "chmod", &[&spec.mode, &mountpoint])
            .await?;
        self.run_success("chown", "chown", &[&spec.owner, &mountpoint])
            .await
    }

    async fn update_quota(&self, dataset: &str, current: &str, requested: &str) -> Result<()> {
        let current_bytes =
            parse_quota_bytes(current).map_err(|err| Error::operation("zfs_quota", err))?;
        let requested_bytes =
            parse_quota_bytes(requested).map_err(|err| Error::operation("zfs_quota", err))?;
        if requested_bytes == current_bytes {
            return Ok(());
        }
        if requested_bytes < current_bytes {
            let used = self.used_bytes(dataset).await?;
            if used > requested_bytes {
                return Err(Error::operation(
                    "zfs_quota",
                    format!(
                        "requested quota {requested} is below current used bytes {used} for dataset '{dataset}'"
                    ),
                ));
            }
        } else if requested_bytes > current_bytes {
            self.check_overcommit(dataset, requested_bytes).await?;
        }
        let quota = format!("quota={requested}");
        self.run_success("zfs set quota", "zfs", &["set", &quota, dataset])
            .await
    }

    async fn check_overcommit(&self, dataset: &str, requested_bytes: u64) -> Result<()> {
        let pool_size = self.pool_size_bytes().await?;
        let other = self.declared_quota_bytes_excluding(dataset).await?;
        let total = other.saturating_add(requested_bytes);
        let budget = (pool_size as f64 * self.overcommit_ratio).floor() as u64;
        if total > budget {
            return Err(Error::operation(
                "zfs_overcommit",
                format!(
                    "quota for '{dataset}' would overcommit pool '{}': declared total {total} bytes exceeds budget {budget} bytes (pool size {pool_size}, ratio {})",
                    self.pool_name(),
                    self.overcommit_ratio,
                ),
            ));
        }
        Ok(())
    }

    fn pool_name(&self) -> &str {
        self.zfs_root_dataset
            .split('/')
            .next()
            .unwrap_or(&self.zfs_root_dataset)
    }

    async fn pool_size_bytes(&self) -> Result<u64> {
        let pool = self.pool_name();
        let output = self
            .runner
            .run("zpool", &["list", "-Hp", "-o", "size", pool])
            .await?;
        ensure_success("zpool list size", &output.stderr, output.status)?;
        let value = parse_single_field(&output.stdout, "pool size")?;
        value
            .parse::<u64>()
            .map_err(|err| Error::operation("zfs_parse", format!("parse pool size: {err}")))
    }

    async fn declared_quota_bytes_excluding(&self, exclude_dataset: &str) -> Result<u64> {
        let output = self
            .runner
            .run(
                "zfs",
                &[
                    "list",
                    "-Hp",
                    "-r",
                    "-o",
                    "name,quota",
                    &self.zfs_root_dataset,
                ],
            )
            .await?;
        ensure_success("zfs list quotas", &output.stderr, output.status)?;
        let text = String::from_utf8(output.stdout)
            .map_err(|err| Error::operation("zfs_parse", err.to_string()))?;
        let mut sum: u64 = 0;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parts = trimmed.split('\t').collect::<Vec<_>>();
            let [name, quota] = parts.as_slice() else {
                return Err(Error::operation(
                    "zfs_parse",
                    format!("expected name and quota for line '{trimmed}'"),
                ));
            };
            if *name == exclude_dataset || *name == self.zfs_root_dataset {
                continue;
            }
            // `zfs list -Hp -o quota` prints "0" for unset (and "-" on some
            // dataset types). Anything else must parse as bytes; failing
            // closed prevents a malformed value from silently relaxing the
            // overcommit guard.
            let bytes = match quota.trim() {
                "" | "-" => 0,
                value => value.parse::<u64>().map_err(|err| {
                    Error::operation(
                        "zfs_parse",
                        format!("parse quota for '{name}': {err} (got {value:?})"),
                    )
                })?,
            };
            sum = sum.saturating_add(bytes);
        }
        Ok(sum)
    }

    async fn used_bytes(&self, dataset: &str) -> Result<u64> {
        let output = self
            .runner
            .run("zfs", &["get", "-Hp", "-o", "value", "used", dataset])
            .await?;
        ensure_success("zfs get used", &output.stderr, output.status)?;
        let value = parse_single_field(&output.stdout, "used bytes")?;
        value
            .parse::<u64>()
            .map_err(|err| Error::operation("zfs_parse", format!("parse used bytes: {err}")))
    }

    async fn run_success(&self, context: &'static str, program: &str, args: &[&str]) -> Result<()> {
        let output = self.runner.run(program, args).await?;
        ensure_success(context, &output.stderr, output.status)
    }
}

impl ZfsDriver<TokioShellRunner> {
    pub fn spawn_send_full(&self, dataset: &str, snapshot: &str) -> Result<tokio::process::Child> {
        let full = snapshot_name(dataset, snapshot);
        self.runner
            .spawn("zfs", &["send", &full], ShellStdio::PipedStdout)
    }

    pub fn spawn_send_incremental(
        &self,
        dataset: &str,
        from_snapshot: &str,
        snapshot: &str,
    ) -> Result<tokio::process::Child> {
        let from = snapshot_name(dataset, from_snapshot);
        let to = snapshot_name(dataset, snapshot);
        self.runner
            .spawn("zfs", &["send", "-i", &from, &to], ShellStdio::PipedStdout)
    }

    /// Spawn `zfs recv` against `dataset`. We deliberately do not pass `-F` so
    /// that an unexpected pre-existing dataset or divergent snapshot list is
    /// surfaced as an error rather than silently rolled back. Callers are
    /// responsible for pre-checking idempotency (see
    /// [`ZfsDriver::snapshot_exists`]) and cleaning up partial state on
    /// failure (see [`ZfsDriver::destroy_snapshot`]).
    pub fn spawn_recv(&self, dataset: &str) -> Result<tokio::process::Child> {
        self.runner
            .spawn("zfs", &["recv", dataset], ShellStdio::PipedStdin)
    }
}

fn ensure_success(context: &'static str, stderr: &[u8], status: i32) -> Result<()> {
    if status == 0 {
        return Ok(());
    }
    Err(Error::operation(
        context,
        command_failure_message(stderr, status),
    ))
}

fn command_failure_message(stderr: &[u8], status: i32) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        format!("exited with status {status}")
    } else {
        message
    }
}

fn zfs_reports_not_found(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    message.contains("dataset does not exist") || message.contains("snapshot does not exist")
}

fn parse_single_field(stdout: &[u8], field: &str) -> Result<String> {
    let text = String::from_utf8(stdout.to_vec())
        .map_err(|err| Error::operation("zfs_parse", err.to_string()))?;
    let value = text.trim();
    if value.is_empty() {
        return Err(Error::operation("zfs_parse", format!("missing {field}")));
    }
    Ok(value.to_string())
}

fn normalize_quota(value: &str) -> String {
    if value == "-" {
        String::new()
    } else {
        value.to_string()
    }
}

fn snapshot_name(dataset: &str, snapshot: &str) -> String {
    format!("{dataset}@{snapshot}")
}

fn mode_matches(actual: &str, expected: &str) -> bool {
    parse_octal_mode(actual)
        .zip(parse_octal_mode(expected))
        .is_some_and(|(a, e)| a == e)
        || actual == expected
}

fn parse_octal_mode(value: &str) -> Option<u32> {
    let trimmed = value.trim_start_matches('0');
    let normalized = if trimmed.is_empty() { "0" } else { trimmed };
    u32::from_str_radix(normalized, 8).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::storage::ShellOutput;

    #[derive(Debug, Clone, Default)]
    struct FakeShellRunner {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        outputs: Arc<Mutex<VecDeque<ShellOutput>>>,
    }

    impl FakeShellRunner {
        fn push(&self, status: i32, stdout: &str, stderr: &str) {
            self.outputs
                .lock()
                .expect("outputs")
                .push_back(ShellOutput {
                    status,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: stderr.as_bytes().to_vec(),
                });
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls").clone()
        }
    }

    #[async_trait]
    impl ShellRunner for FakeShellRunner {
        async fn run(&self, program: &str, args: &[&str]) -> Result<ShellOutput> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().map(|arg| (*arg).to_string()));
            self.calls.lock().expect("calls").push(call);
            self.outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .ok_or_else(|| Error::operation("fake_shell", "missing output"))
        }
    }

    fn spec() -> DatasetSpec {
        DatasetSpec {
            dataset: "tank/ployz/prod/data".into(),
            mountpoint: "/tank/ployz/prod/data".into(),
            quota: "1G".into(),
            mode: "0750".into(),
            owner: "999:999".into(),
        }
    }

    async fn driver_with_ratio(fake: &FakeShellRunner, ratio: f64) -> ZfsDriver<FakeShellRunner> {
        fake.push(0, "/tank/ployz\n", "");
        ZfsDriver::new(fake.clone(), "tank/ployz", ratio)
            .await
            .expect("driver")
    }

    async fn driver(fake: &FakeShellRunner) -> ZfsDriver<FakeShellRunner> {
        driver_with_ratio(fake, 1.0).await
    }

    fn push_overcommit_lookup(fake: &FakeShellRunner, pool_size_bytes: u64, quota_list: &str) {
        fake.push(0, &format!("{pool_size_bytes}\n"), "");
        fake.push(0, quota_list, "");
    }

    #[tokio::test]
    async fn ensure_creates_missing_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls[1][..4], ["zfs", "list", "-Hp", "-o"]);
        assert_eq!(calls[2][..3], ["zpool", "list", "-Hp"]);
        assert_eq!(calls[3][..4], ["zfs", "list", "-Hp", "-r"]);
        assert_eq!(calls[4][..4], ["zfs", "create", "-p", "-o"]);
        assert_eq!(calls[5], ["chmod", "0750", "/tank/ployz/prod/data"]);
        assert_eq!(calls[6], ["chown", "999:999", "/tank/ployz/prod/data"]);
    }

    #[tokio::test]
    async fn ensure_propagates_zfs_list_failure() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "permission denied");

        let err = driver
            .ensure(&spec())
            .await
            .expect_err("zfs list failure should fail");

        assert!(err.to_string().contains("permission denied"), "got: {err}");
        assert_eq!(fake.calls().len(), 2);
    }

    #[tokio::test]
    async fn ensure_adopts_matching_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "750:999:999\n", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[2],
            ["stat", "-c", "%a:%u:%g", "/tank/ployz/prod/data"]
        );
    }

    #[tokio::test]
    async fn ensure_reconciles_drifted_mode_and_owner_on_adopt() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "777:0:0\n", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(
            calls[2],
            ["stat", "-c", "%a:%u:%g", "/tank/ployz/prod/data"]
        );
        assert_eq!(calls[3], ["chmod", "0750", "/tank/ployz/prod/data"]);
        assert_eq!(calls[4], ["chown", "999:999", "/tank/ployz/prod/data"]);
    }

    #[tokio::test]
    async fn ensure_skips_quota_update_when_numeric_zfs_quota_matches_request() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "750:999:999\n", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[1],
            vec![
                "zfs",
                "list",
                "-Hp",
                "-o",
                "name,quota,mountpoint",
                "tank/ployz/prod/data"
            ]
        );
    }

    #[tokio::test]
    async fn ensure_grows_quota() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(4),
            "tank/ployz\t0\ntank/ployz/prod/data\t1073741824\n",
        );
        fake.push(0, "", "");
        fake.push(0, "750:999:999\n", "");
        let mut next = spec();
        next.quota = "2G".into();

        driver.ensure(&next).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls[4], ["zfs", "set", "quota=2G", "tank/ployz/prod/data"]);
        assert_eq!(
            calls[5],
            ["stat", "-c", "%a:%u:%g", "/tank/ployz/prod/data"]
        );
    }

    #[tokio::test]
    async fn ensure_refuses_shrink_below_used() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t2147483648\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "1610612736\n", "");

        let err = driver
            .ensure(&spec())
            .await
            .expect_err("shrink should fail");

        assert!(err.to_string().contains("below current used bytes"));
    }

    #[tokio::test]
    async fn ensure_rejects_mountpoint_mismatch() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/other\n", "");

        let err = driver
            .ensure(&spec())
            .await
            .expect_err("mismatch should fail");

        assert!(err.to_string().contains("has mountpoint"));
    }

    #[tokio::test]
    async fn snapshot_creation_reads_guid() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "snapshot does not exist");
        fake.push(0, "", "");
        fake.push(0, "42\n", "");

        let snapshot = driver
            .create_snapshot("tank/ployz/prod/data", "base")
            .await
            .expect("snapshot");

        assert_eq!(snapshot.name, "base");
        assert_eq!(snapshot.guid, 42);
        let calls = fake.calls();
        assert_eq!(
            calls[1],
            [
                "zfs",
                "list",
                "-H",
                "-t",
                "snapshot",
                "-o",
                "name",
                "tank/ployz/prod/data@base"
            ]
        );
        assert_eq!(calls[2], ["zfs", "snapshot", "tank/ployz/prod/data@base"]);
        assert_eq!(
            calls[3],
            [
                "zfs",
                "get",
                "-Hp",
                "-o",
                "value",
                "guid",
                "tank/ployz/prod/data@base"
            ]
        );
    }

    #[tokio::test]
    async fn snapshot_exists_returns_errors_other_than_not_found() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "permission denied");

        let err = driver
            .snapshot_exists("tank/ployz/prod/data", "base")
            .await
            .expect_err("permission failure should not be treated as missing");

        assert!(err.to_string().contains("permission denied"));
    }

    #[tokio::test]
    async fn inspect_dataset_returns_snapshot_lineage() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/tank/ployz/prod/data\n", "");
        fake.push(0, "123\n", "");
        fake.push(
            0,
            "tank/ployz/prod/data@base\t42\ntank/ployz/prod/data@cutover\t43\n",
            "",
        );

        let info = driver
            .inspect_dataset("tank/ployz/prod/data")
            .await
            .expect("inspect");

        assert_eq!(info.used_bytes, 123);
        assert_eq!(
            info.snapshots,
            vec![
                SnapshotInfo {
                    name: "base".into(),
                    guid: 42
                },
                SnapshotInfo {
                    name: "cutover".into(),
                    guid: 43
                }
            ]
        );
    }

    #[tokio::test]
    async fn ensure_parent_dataset_creates_when_missing() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // dataset_exists -> read_dataset returns missing
        fake.push(1, "", "dataset does not exist");
        // zfs create -p success
        fake.push(0, "", "");

        driver
            .ensure_parent_dataset("tank/ployz/prod/data")
            .await
            .expect("ensure parent");

        let calls = fake.calls();
        assert_eq!(
            calls[1],
            [
                "zfs",
                "list",
                "-Hp",
                "-o",
                "name,quota,mountpoint",
                "tank/ployz/prod"
            ]
        );
        assert_eq!(calls[2], ["zfs", "create", "-p", "tank/ployz/prod"]);
    }

    #[tokio::test]
    async fn ensure_parent_dataset_no_op_when_parent_is_root() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        driver
            .ensure_parent_dataset("tank/ployz/data")
            .await
            .expect("ensure parent root");
        // Only the root validation call from driver construction.
        assert_eq!(fake.calls().len(), 1);
    }

    #[tokio::test]
    async fn destroy_snapshot_treats_missing_as_success() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            1,
            "",
            "could not find any snapshots to destroy: dataset does not exist",
        );
        driver
            .destroy_snapshot("tank/ployz/prod/data", "missing")
            .await
            .expect("destroy missing snapshot");
    }

    #[tokio::test]
    async fn ensure_rejects_overcommit_on_create() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(3),
            "tank/ployz\t0\ntank/ployz/other\t536870912\n",
        );
        let mut greedy = spec();
        greedy.quota = "1G".into();

        let err = driver
            .ensure(&greedy)
            .await
            .expect_err("overcommit should fail");

        assert!(
            err.to_string().contains("would overcommit pool"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn ensure_rejects_overcommit_on_grow() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/tank/ployz/prod/data\n", "");
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(3),
            "tank/ployz\t0\ntank/ployz/prod/data\t1073741824\ntank/ployz/other\t536870912\n",
        );
        let mut next = spec();
        next.quota = "1G".into();
        // Force the grow path: existing 1G in the read result, requested 2G
        next.quota = "2G".into();

        let err = driver
            .ensure(&next)
            .await
            .expect_err("overcommit on grow should fail");

        assert!(
            err.to_string().contains("would overcommit pool"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn ensure_respects_ratio_above_one() {
        let fake = FakeShellRunner::default();
        let driver = driver_with_ratio(&fake, 2.0).await;
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(3),
            "tank/ployz\t0\ntank/ployz/other\t1073741824\n",
        );
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");
        let mut greedy = spec();
        greedy.quota = "1G".into();

        driver
            .ensure(&greedy)
            .await
            .expect("ratio 2.0 should allow");
    }

    #[tokio::test]
    async fn new_rejects_invalid_ratio() {
        let fake = FakeShellRunner::default();
        let err = ZfsDriver::new(fake, "tank/ployz", 0.0)
            .await
            .expect_err("zero ratio should be rejected");
        assert!(err.to_string().contains("overcommit_ratio"));
    }
}
