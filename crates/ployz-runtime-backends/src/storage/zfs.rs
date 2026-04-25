use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::ShellRunner;

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

#[derive(Debug, Clone)]
pub struct ZfsDriver<R> {
    runner: R,
    zfs_root_dataset: String,
    zfs_root_mountpoint: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExistingDataset {
    quota: String,
    mountpoint: PathBuf,
}

impl<R: ShellRunner> ZfsDriver<R> {
    pub async fn new(runner: R, zfs_root: &str) -> Result<Self> {
        let output = runner
            .run("zfs", &["list", "-H", "-o", "mountpoint", zfs_root])
            .await?;
        ensure_success("zfs list root", &output.stderr, output.status)?;
        let mountpoint = parse_single_field(&output.stdout, "root mountpoint")?;
        Ok(Self {
            runner,
            zfs_root_dataset: zfs_root.to_string(),
            zfs_root_mountpoint: PathBuf::from(mountpoint),
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
            }
            None => self.create_dataset(spec).await?,
        }
        Ok(MountInfo {
            mountpoint: spec.mountpoint.clone(),
        })
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
                &["list", "-H", "-o", "name,quota,mountpoint", dataset],
            )
            .await?;
        if output.status != 0 {
            return Ok(None);
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
        let current_bytes = parse_size_bytes(current)?;
        let requested_bytes = parse_size_bytes(requested)?;
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
        }
        let quota = format!("quota={requested}");
        self.run_success("zfs set quota", "zfs", &["set", &quota, dataset])
            .await
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

fn ensure_success(context: &'static str, stderr: &[u8], status: i32) -> Result<()> {
    if status == 0 {
        return Ok(());
    }
    Err(Error::operation(
        context,
        String::from_utf8_lossy(stderr).trim().to_string(),
    ))
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

fn parse_size_bytes(value: &str) -> Result<u64> {
    let Some(last) = value.chars().last() else {
        return Err(Error::operation("zfs_quota", "empty quota"));
    };
    let (digits, multiplier) = match last {
        'K' => (&value[..value.len() - 1], 1024_u64),
        'M' => (&value[..value.len() - 1], 1024_u64.pow(2)),
        'G' => (&value[..value.len() - 1], 1024_u64.pow(3)),
        'T' => (&value[..value.len() - 1], 1024_u64.pow(4)),
        _ => (value, 1),
    };
    let amount = digits
        .parse::<u64>()
        .map_err(|err| Error::operation("zfs_quota", format!("parse quota '{value}': {err}")))?;
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| Error::operation("zfs_quota", format!("quota '{value}' is too large")))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::storage::ShellOutput;

    #[derive(Clone, Default)]
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

    async fn driver(fake: &FakeShellRunner) -> ZfsDriver<FakeShellRunner> {
        fake.push(0, "/tank/ployz\n", "");
        ZfsDriver::new(fake.clone(), "tank/ployz")
            .await
            .expect("driver")
    }

    #[tokio::test]
    async fn ensure_creates_missing_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "dataset does not exist");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls[1][..4], ["zfs", "list", "-H", "-o"]);
        assert_eq!(calls[2][..4], ["zfs", "create", "-p", "-o"]);
        assert_eq!(calls[3], ["chmod", "0750", "/tank/ployz/prod/data"]);
        assert_eq!(calls[4], ["chown", "999:999", "/tank/ployz/prod/data"]);
    }

    #[tokio::test]
    async fn ensure_adopts_matching_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/tank/ployz/prod/data\n", "");

        driver.ensure(&spec()).await.expect("ensure");

        assert_eq!(fake.calls().len(), 2);
    }

    #[tokio::test]
    async fn ensure_grows_quota() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/tank/ployz/prod/data\n", "");
        fake.push(0, "", "");
        let mut next = spec();
        next.quota = "2G".into();

        driver.ensure(&next).await.expect("ensure");

        assert_eq!(
            fake.calls().last().expect("last"),
            &vec!["zfs", "set", "quota=2G", "tank/ployz/prod/data"]
        );
    }

    #[tokio::test]
    async fn ensure_refuses_shrink_below_used() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t2G\t/tank/ployz/prod/data\n", "");
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
}
