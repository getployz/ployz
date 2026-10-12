//! The receive task: one stream into a slot's `fs`, from the writer Machine or, for a
//! seed, from a local snapshot.

use std::{
    collections::BTreeMap,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt as _;
use ployz_core::{ManagementAddress, SendStream};
use tokio::{io::AsyncWriteExt as _, process::Command};

use super::{DockerVolumeName, VolumeStorage};

/// A receive's `kill-daemon` fault fires once this much of the stream is in, so ZFS holds a
/// partial receive to resume.
const KILL_AFTER_BYTES: u64 = 1 << 20;

pub(super) const RECEIVE_STALL: Duration = Duration::from_secs(60);

/// The receive tasks this process started, by Volume name; a finished task keeps its
/// outcome until the next one replaces it.
#[derive(Clone, Default)]
pub(super) struct Receives(Arc<Mutex<BTreeMap<String, ReceiveTask>>>);

struct ReceiveTask {
    outcome: Arc<Mutex<Option<Result<(), String>>>>,
}

impl Receives {
    pub(super) fn running(&self, name: &DockerVolumeName) -> bool {
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .get(&name.0)
            .is_some_and(|task| task.outcome.lock().expect("outcome lock").is_none())
    }

    pub(super) fn failure(&self, name: &DockerVolumeName) -> Option<String> {
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .get(&name.0)
            .and_then(|task| task.outcome.lock().expect("outcome lock").clone())
            .and_then(Result::err)
    }

    pub(super) fn forget(&self, name: &DockerVolumeName) {
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .remove(&name.0);
    }

    pub(super) fn start(&self, name: &DockerVolumeName) -> Arc<Mutex<Option<Result<(), String>>>> {
        let outcome = Arc::new(Mutex::new(None));
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .insert(
                name.0.clone(),
                ReceiveTask {
                    outcome: Arc::clone(&outcome),
                },
            );
        outcome
    }
}

/// Why a receive did not land its stream.
#[derive(Debug)]
pub(super) enum ReceiveFailure {
    /// The writer has no such stream: the base or the token is gone from it.
    NoStream(String),
    Failed(String),
}

impl std::fmt::Display for ReceiveFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoStream(message) | Self::Failed(message) => f.write_str(message),
        }
    }
}

impl VolumeStorage {
    /// Pulls one stream into `fs`, then re-asserts and verifies the slot's bound and
    /// read-only state, which ZFS drops on an interrupted first receive. `fault` names
    /// the kill point that fires once the stream passes [`KILL_AFTER_BYTES`].
    pub(super) async fn receive(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
        fs: &str,
        bound: u64,
        fault: &'static str,
    ) -> Result<(), ReceiveFailure> {
        let url = format!(
            "http://[{}]:{}{}",
            from.0,
            self.send_port,
            stream.path_and_query()
        );
        let client = reqwest::Client::builder()
            .connect_timeout(self.receive_stall)
            .read_timeout(self.receive_stall)
            .build()
            .map_err(|error| {
                ReceiveFailure::Failed(format!("could not build the receive client: {error}"))
            })?;
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|error| ReceiveFailure::Failed(format!("GET {url}: {error}")))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            let reason = response.text().await.unwrap_or_default();
            return Err(ReceiveFailure::NoStream(format!("GET {url}: {reason}")));
        }
        let response = response
            .error_for_status()
            .map_err(|error| ReceiveFailure::Failed(format!("GET {url}: {error}")))?;
        let refquota = format!("refquota={bound}");
        let mut child = Command::new(&self.zfs)
            .args([
                "receive",
                "-u",
                "-s",
                "-o",
                "readonly=on",
                "-o",
                &refquota,
                fs,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                ReceiveFailure::Failed(format!("could not run zfs receive: {error}"))
            })?;
        let mut stdin = child.stdin.take().expect("zfs receive stdin is piped");
        let mut body = response.bytes_stream();
        let mut received = 0u64;
        let mut broke = None;
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => {
                    match tokio::time::timeout(self.receive_stall, stdin.write_all(&bytes)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            broke = Some(error.to_string());
                            break;
                        }
                        Err(_) => {
                            return Err(ReceiveFailure::Failed(format!(
                                "zfs receive into {fs} stalled writing after {received} bytes"
                            )));
                        }
                    }
                    let before = received;
                    received += bytes.len() as u64;
                    if before < KILL_AFTER_BYTES && received >= KILL_AFTER_BYTES {
                        ployzd::faults::kill_inside(fault);
                    }
                }
                Err(error) => {
                    broke = Some(error.to_string());
                    break;
                }
            }
        }
        drop(stdin);
        let output = tokio::time::timeout(self.receive_stall, child.wait_with_output())
            .await
            .map_err(|_| {
                ReceiveFailure::Failed(format!(
                    "zfs receive into {fs} stalled finishing after {received} bytes"
                ))
            })?
            .map_err(|error| {
                ReceiveFailure::Failed(format!("zfs receive did not finish: {error}"))
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ReceiveFailure::Failed(format!(
                "zfs receive into {fs} failed after {received} bytes: {}{}",
                stderr.trim(),
                broke.map_or(String::new(), |error| format!(" (stream: {error})"))
            )));
        }
        if let Some(error) = broke {
            return Err(ReceiveFailure::Failed(format!(
                "stream from {url} broke after {received} bytes: {error}"
            )));
        }
        self.finish_copy(fs, bound)
            .await
            .map_err(ReceiveFailure::Failed)
    }

    pub(super) async fn finish_copy(&self, fs: &str, bound: u64) -> Result<(), String> {
        let refquota = format!("refquota={bound}");
        for property in ["readonly=on", &refquota] {
            self.zfs(&["set", property, fs])
                .await
                .map_err(|error| error.to_string())?;
        }
        self.verify_copy(fs, bound).await
    }

    /// The copy is read-only and carries the slot's bound, whatever the stream said.
    /// `-p` keeps the bound in bytes; `zfs get` rounds it to `3G` otherwise.
    async fn verify_copy(&self, fs: &str, bound: u64) -> Result<(), String> {
        for (property, expected) in [("readonly", "on"), ("refquota", bound.to_string().as_str())] {
            let actual = self
                .zfs(&["get", "-Hp", "-o", "value", property, fs])
                .await
                .map_err(|error| error.to_string())?;
            let actual = actual.trim_end_matches('\n');
            if actual != expected {
                return Err(format!(
                    "{fs} has {property}={actual} after the receive; expected {expected}"
                ));
            }
        }
        Ok(())
    }
}
