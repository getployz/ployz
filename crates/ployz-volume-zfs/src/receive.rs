use std::path::PathBuf;

use crate::{
    ShellRunner, TokioShellRunner, ZfsDriver, ZfsReceiveRequest, ZfsTransferValidationError,
};

#[derive(Debug, PartialEq, Eq)]
enum ReceiveDecision {
    AlreadyHave(u64),
    Proceed { cleanup: ReceiveFailureCleanup },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiveFailureCleanup {
    SnapshotOnly,
}

pub async fn receive_stream<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    zfs_root: &PathBuf,
    overcommit_ratio: f64,
    request: &ZfsReceiveRequest,
) -> Result<u64, String> {
    let root = zfs_root
        .to_str()
        .ok_or_else(|| format!("zfs_root is not valid UTF-8: {}", zfs_root.display()))?;
    let driver = ZfsDriver::new(TokioShellRunner, root, overcommit_ratio)
        .await
        .map_err(|error| error.to_string())?;
    let dataset = format!(
        "{root}/{}/{}",
        request.namespace().as_str(),
        request.volume()
    );

    let cleanup = match prepare_receive(&driver, &dataset, request)
        .await
        .map_err(|error| error.to_string())?
    {
        ReceiveDecision::AlreadyHave(guid) => {
            // Drain whatever the source already started to write so the source
            // process exits cleanly instead of breaking on EPIPE.
            let _ = tokio::io::copy(reader, &mut tokio::io::sink()).await;
            return Ok(guid);
        }
        ReceiveDecision::Proceed { cleanup } => cleanup,
    };

    let mut recv = driver
        .spawn_recv(&dataset)
        .map_err(|error| error.to_string())?;
    let Some(mut stdin) = recv.stdin.take() else {
        return Err("zfs recv stdin was not piped".to_string());
    };
    let copy_result = tokio::io::copy(reader, &mut stdin).await;
    drop(stdin);
    let output = recv
        .wait_with_output()
        .await
        .map_err(|error| format!("wait for zfs recv: {error}"))?;
    if let Err(error) = copy_result {
        cleanup_partial(&driver, &dataset, request, cleanup).await;
        return Err(format!("copy stream into zfs recv: {error}"));
    }
    if !output.status.success() {
        cleanup_partial(&driver, &dataset, request, cleanup).await;
        return Err(format!(
            "zfs recv failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    driver
        .snapshot_guid(&dataset, request.snapshot())
        .await
        .map_err(|error| error.to_string())
}

/// Pure decision logic for an incoming `zfs recv`. Generic over `ShellRunner`
/// so it can be unit-tested with a fake runner; the real `receive_stream`
/// wraps this and performs the actual stream I/O.
async fn prepare_receive<R: ShellRunner>(
    driver: &ZfsDriver<R>,
    dataset: &str,
    request: &ZfsReceiveRequest,
) -> Result<ReceiveDecision, ZfsTransferValidationError> {
    // Idempotency: if a previous attempt already landed this snapshot with
    // the right GUID, the caller drains the source stream and returns the
    // guid without touching ZFS.
    if driver
        .snapshot_exists(dataset, request.snapshot())
        .await
        .map_err(|error| ZfsTransferValidationError::backend("snapshot_exists", error))?
    {
        let guid = driver
            .snapshot_guid(dataset, request.snapshot())
            .await
            .map_err(|error| ZfsTransferValidationError::backend("snapshot_guid", error))?;
        if guid == request.expected_guid() {
            return Ok(ReceiveDecision::AlreadyHave(guid));
        }
        return Err(ZfsTransferValidationError::ExistingSnapshotGuidMismatch {
            dataset: dataset.to_string(),
            snapshot: request.snapshot().to_string(),
            actual_guid: guid,
            expected_guid: request.expected_guid(),
        });
    }

    // For incrementals, refuse to recv unless the named base snapshot is
    // already on disk with the GUID the source claims it had. Catches the
    // "wrong base" footgun that `zfs recv` would otherwise surface as a
    // confusing checksum/lineage error mid-stream.
    let cleanup = if let Some(from_snapshot) = request.from_snapshot() {
        if !driver
            .snapshot_exists(dataset, from_snapshot)
            .await
            .map_err(|error| ZfsTransferValidationError::backend("snapshot_exists", error))?
        {
            return Err(ZfsTransferValidationError::IncrementalBaseMissing {
                dataset: dataset.to_string(),
                snapshot: from_snapshot.to_string(),
            });
        }
        if let Some(expected_from_guid) = request.from_snapshot_guid() {
            let actual = driver
                .snapshot_guid(dataset, from_snapshot)
                .await
                .map_err(|error| ZfsTransferValidationError::backend("snapshot_guid", error))?;
            if actual != expected_from_guid {
                return Err(ZfsTransferValidationError::IncrementalBaseGuidMismatch {
                    dataset: dataset.to_string(),
                    snapshot: from_snapshot.to_string(),
                    actual_guid: actual,
                    expected_guid: expected_from_guid,
                });
            }
        }
        ReceiveFailureCleanup::SnapshotOnly
    } else {
        // Full sends require the parent namespace dataset to exist; `zfs recv`
        // does not auto-create ancestors.
        driver
            .ensure_parent_dataset(dataset)
            .await
            .map_err(|error| ZfsTransferValidationError::backend("ensure_parent_dataset", error))?;
        ReceiveFailureCleanup::SnapshotOnly
    };
    Ok(ReceiveDecision::Proceed { cleanup })
}

/// Best-effort cleanup of partial state left behind by a failed `zfs recv`.
/// Dataset cleanup is deliberately conservative. A failed receive may leave an
/// empty dataset behind, but deleting recursively would require an ownership
/// marker or per-dataset receive lock to prove the dataset still belongs to
/// this transfer.
async fn cleanup_partial<R: ShellRunner>(
    driver: &ZfsDriver<R>,
    dataset: &str,
    request: &ZfsReceiveRequest,
    cleanup: ReceiveFailureCleanup,
) {
    if let Err(error) = driver.destroy_snapshot(dataset, request.snapshot()).await {
        tracing::warn!(
            %error,
            dataset,
            snapshot = %request.snapshot(),
            "failed to clean up partial snapshot after recv failure"
        );
    }
    let _ = cleanup;
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use crate::{
        ShellOutput, ShellRunner, ZfsDriver, ZfsReceiveRequest, ZfsTransferOpen,
        ZfsTransferValidationError,
    };
    use async_trait::async_trait;
    use ployz_error::{Error, Result};
    use ployz_model::MachineId;

    use super::{ReceiveDecision, ReceiveFailureCleanup, cleanup_partial, prepare_receive};

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

    /// Build a driver wired to fake `tank/ployz` root. The driver constructor
    /// itself runs one `zfs list` call, so we satisfy that first.
    async fn driver(fake: &FakeShellRunner) -> ZfsDriver<FakeShellRunner> {
        fake.push(0, "/tank/ployz\n", "");
        ZfsDriver::new(fake.clone(), "tank/ployz", 1.0)
            .await
            .expect("driver")
    }

    fn open(snapshot: &str, expected_guid: u64) -> ZfsTransferOpen {
        ZfsTransferOpen {
            namespace: "default".into(),
            volume: "data".into(),
            snapshot: snapshot.into(),
            expected_guid,
            source_machine_id: Some(MachineId::new("source")),
            from_snapshot: None,
            from_snapshot_guid: None,
        }
    }

    fn request(snapshot: &str, expected_guid: u64) -> ZfsReceiveRequest {
        receive_request(open(snapshot, expected_guid))
    }

    fn receive_request(open: ZfsTransferOpen) -> ZfsReceiveRequest {
        ZfsReceiveRequest::from_authorized_open(
            &open,
            ployz_spec::Namespace::try_new("default").expect("namespace"),
        )
    }

    #[tokio::test]
    async fn cleanup_partial_full_receive_keeps_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "", "");

        cleanup_partial(
            &driver,
            "tank/ployz/default/data",
            &request("snap", 42),
            ReceiveFailureCleanup::SnapshotOnly,
        )
        .await;

        let calls = fake.calls();
        assert_eq!(calls[1], ["zfs", "destroy", "tank/ployz/default/data@snap"]);
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn cleanup_partial_incremental_receive_keeps_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "", "");
        let mut transfer = open("snap", 42);
        transfer.from_snapshot = Some("base".into());
        transfer.from_snapshot_guid = Some(11);
        let request = receive_request(transfer);

        cleanup_partial(
            &driver,
            "tank/ployz/default/data",
            &request,
            ReceiveFailureCleanup::SnapshotOnly,
        )
        .await;

        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], ["zfs", "destroy", "tank/ployz/default/data@snap"]);
    }

    #[tokio::test]
    async fn prepare_receive_returns_already_have_when_snapshot_matches() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // snapshot_exists -> status 0 means present
        fake.push(0, "tank/ployz/default/data@snap\n", "");
        // snapshot_guid -> stdout is the guid
        fake.push(0, "42\n", "");

        let decision = prepare_receive(&driver, "tank/ployz/default/data", &request("snap", 42))
            .await
            .expect("prepare");

        assert_eq!(decision, ReceiveDecision::AlreadyHave(42));
    }

    #[tokio::test]
    async fn prepare_receive_rejects_existing_snapshot_with_mismatched_guid() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/default/data@snap\n", "");
        fake.push(0, "7\n", "");

        let err = prepare_receive(&driver, "tank/ployz/default/data", &request("snap", 42))
            .await
            .expect_err("mismatched guid should fail");

        assert!(matches!(
            err,
            ZfsTransferValidationError::ExistingSnapshotGuidMismatch {
                actual_guid: 7,
                expected_guid: 42,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn prepare_receive_rejects_incremental_when_base_missing() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // base snapshot also missing
        fake.push(1, "", "snapshot does not exist");
        let mut o = open("snap", 42);
        o.from_snapshot = Some("base".into());
        o.from_snapshot_guid = Some(11);
        let request = receive_request(o);

        let err = prepare_receive(&driver, "tank/ployz/default/data", &request)
            .await
            .expect_err("missing base should fail");

        assert!(matches!(
            err,
            ZfsTransferValidationError::IncrementalBaseMissing { .. }
        ));
    }

    #[tokio::test]
    async fn prepare_receive_rejects_incremental_when_base_guid_diverges() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // base snapshot present
        fake.push(0, "tank/ployz/default/data@base\n", "");
        // base guid -> different from claim
        fake.push(0, "99\n", "");
        let mut o = open("snap", 42);
        o.from_snapshot = Some("base".into());
        o.from_snapshot_guid = Some(11);
        let request = receive_request(o);

        let err = prepare_receive(&driver, "tank/ployz/default/data", &request)
            .await
            .expect_err("mismatched base guid should fail");

        assert!(matches!(
            err,
            ZfsTransferValidationError::IncrementalBaseGuidMismatch {
                actual_guid: 99,
                expected_guid: 11,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn prepare_receive_keeps_existing_dataset_on_full_receive_failure() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // parent lookup performed by ensure_parent_dataset
        fake.push(0, "tank/ployz/default\t1G\t/tank/ployz/default\n", "");

        let decision = prepare_receive(&driver, "tank/ployz/default/data", &request("snap", 42))
            .await
            .expect("prepare");

        assert_eq!(
            decision,
            ReceiveDecision::Proceed {
                cleanup: ReceiveFailureCleanup::SnapshotOnly
            }
        );
    }

    #[tokio::test]
    async fn prepare_receive_keeps_new_full_receive_dataset_on_failure() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // parent lookup performed by ensure_parent_dataset
        fake.push(0, "tank/ployz/default\t1G\t/tank/ployz/default\n", "");

        let decision = prepare_receive(&driver, "tank/ployz/default/data", &request("snap", 42))
            .await
            .expect("prepare");

        assert_eq!(
            decision,
            ReceiveDecision::Proceed {
                cleanup: ReceiveFailureCleanup::SnapshotOnly
            }
        );
    }
}
