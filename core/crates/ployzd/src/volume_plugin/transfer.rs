//! The receive task: one stream into a slot's `fs`, from the writer Machine or, for a
//! seed, from a local snapshot.

use std::{
    collections::BTreeMap,
    future::Future,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{FutureExt as _, StreamExt as _};
use ployz_core::{ManagementAddress, SendStream};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

use super::{DockerVolumeName, VolumeStorage};

/// A receive's `kill-daemon` fault fires once this much of the stream is in, so ZFS holds a
/// partial receive to resume.
const KILL_AFTER_BYTES: u64 = 1 << 20;

pub(super) const RECEIVE_STALL: Duration = Duration::from_secs(60);

/// One deadline for all of a departure's stops, well inside Demote's 120 s timeout.
pub(super) const STOP_DEADLINE: Duration = Duration::from_secs(30);

/// The receive tasks this process started, by Volume name; a finished task keeps its
/// outcome until the next one replaces it.
#[derive(Clone, Default)]
pub(super) struct Receives(Arc<Mutex<BTreeMap<String, ReceiveTask>>>);

struct ReceiveTask {
    outcome: Arc<Mutex<Option<Result<(), String>>>>,
    stop: CancellationToken,
    finished: CancellationToken,
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

    /// Runs `work` as the receive task of `name`. `work` gets the token
    /// [`Self::stop_all`] cancels; once it fires, `work` kills and reaps its `zfs`
    /// children before returning. A panic is recorded as the task's failure.
    pub(super) fn spawn<F>(
        &self,
        name: &DockerVolumeName,
        work: impl FnOnce(CancellationToken) -> F,
    ) where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let task = ReceiveTask {
            outcome: Arc::default(),
            stop: CancellationToken::new(),
            finished: CancellationToken::new(),
        };
        let outcome = Arc::clone(&task.outcome);
        let finished = task.finished.clone().drop_guard();
        let work = work(task.stop.clone());
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .insert(name.0.clone(), task);
        tokio::spawn(async move {
            let _finished = finished;
            let result = std::panic::AssertUnwindSafe(work)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("the receive task panicked".to_owned()));
            *outcome.lock().expect("outcome lock") = Some(result);
        });
    }

    /// Stops every receive task and waits until each ended, so ZFS holds no slot busy
    /// and no receive changes a slot afterwards. Answers the names still running once
    /// `deadline` passes.
    pub(super) async fn stop_all(&self, deadline: Duration) -> Vec<String> {
        let finished: Vec<(String, CancellationToken)> = self
            .0
            .lock()
            .expect("receive registry is not poisoned")
            .iter()
            .map(|(name, task)| {
                task.stop.cancel();
                (name.clone(), task.finished.clone())
            })
            .collect();
        let deadline = tokio::time::Instant::now() + deadline;
        let mut stuck = Vec::new();
        for (name, finished) in finished {
            if tokio::time::timeout_at(deadline, finished.cancelled())
                .await
                .is_err()
            {
                stuck.push(name);
            }
        }
        stuck
    }
}

/// A stream the writer answered 200 for, not yet read.
pub(super) struct OpenedStream {
    url: String,
    response: reqwest::Response,
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
        stop: &CancellationToken,
    ) -> Result<(), ReceiveFailure> {
        let opened = self.open_stream(from, stream, stop).await?;
        self.receive_opened(opened, fs, bound, fault, stop).await
    }

    /// The writer's answer to `stream`, before anything touches the slot. A writer that
    /// never answers holds the departure only until `stop` fires.
    pub(super) async fn open_stream(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
        stop: &CancellationToken,
    ) -> Result<OpenedStream, ReceiveFailure> {
        tokio::select! {
            biased;
            () = stop.cancelled() => Err(ReceiveFailure::Failed(format!(
                "the request for {} was stopped",
                stream.path_and_query()
            ))),
            opened = self.request_stream(from, stream) => opened,
        }
    }

    async fn request_stream(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
    ) -> Result<OpenedStream, ReceiveFailure> {
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
        Ok(OpenedStream { url, response })
    }

    /// Pulls an opened stream into `fs`. Once `stop` fires, the receive is killed and
    /// reaped before this returns.
    pub(super) async fn receive_opened(
        &self,
        OpenedStream { url, response }: OpenedStream,
        fs: &str,
        bound: u64,
        fault: &'static str,
        stop: &CancellationToken,
    ) -> Result<(), ReceiveFailure> {
        let refquota = format!("refquota={bound}");
        let mut child = ZfsChild::spawn(
            Command::new(&self.zfs)
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
                .stderr(Stdio::piped()),
        )
        .map_err(|error| ReceiveFailure::Failed(format!("could not run zfs receive: {error}")))?;
        let pumped = {
            let pump = self.pump(child.child(), &url, response, fs, fault);
            tokio::select! {
                biased;
                () = stop.cancelled() => None,
                pumped = pump => Some(pumped),
            }
        };
        child.reap().await;
        pumped.ok_or_else(|| {
            ReceiveFailure::Failed(format!("the receive into {fs} was stopped"))
        })??;
        self.finish_copy(fs, bound)
            .await
            .map_err(ReceiveFailure::Failed)
    }

    /// Writes the stream into the receive `child` and waits for it to exit.
    async fn pump(
        &self,
        child: &mut Child,
        url: &str,
        response: reqwest::Response,
        fs: &str,
        fault: &'static str,
    ) -> Result<(), ReceiveFailure> {
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
        let (status, stderr) = tokio::time::timeout(self.receive_stall, exited(child))
            .await
            .map_err(|_| {
                ReceiveFailure::Failed(format!(
                    "zfs receive into {fs} stalled finishing after {received} bytes"
                ))
            })?
            .map_err(|error| {
                ReceiveFailure::Failed(format!("zfs receive did not finish: {error}"))
            })?;
        if !status.success() {
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
        Ok(())
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

/// A `zfs` process that has exited by the time its owner is gone, on every path out:
/// one still exiting holds its dataset busy for the next destroy.
pub(super) struct ZfsChild {
    child: Option<Child>,
    #[cfg(test)]
    spared: bool,
}

impl ZfsChild {
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        command.spawn().map(|child| Self {
            child: Some(child),
            #[cfg(test)]
            spared: false,
        })
    }

    pub(super) fn child(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("a zfs child is held until reaped")
    }

    #[cfg(test)]
    pub(super) fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    /// Leaves the child to exit on its own clock when dropped. No process a test can
    /// start outlives SIGKILL; a `zfs` blocked in the kernel does, and this stands in.
    #[cfg(test)]
    pub(super) fn spare_from_kill(&mut self) {
        self.spared = true;
    }

    /// Kills the process unless it already exited, and waits for it.
    pub(super) async fn reap(mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        match child.kill().await {
            Ok(()) => {}
            // Already waited for.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(error) => tracing::warn!(%error, "could not kill a zfs process"),
        }
    }
}

/// Only an early return or a panic gets here; every other path reaps. Dropping cannot
/// await, so it blocks on the kill, which takes milliseconds.
impl Drop for ZfsChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(test)]
        let spared = self.spared;
        #[cfg(not(test))]
        let spared = false;
        if !spared && child.start_kill().is_err() {
            return;
        }
        for _ in 0..100 {
            match child.try_wait() {
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Ok(Some(_)) | Err(_) => return,
            }
        }
        tracing::warn!(pid = ?child.id(), "a killed zfs process did not exit within a second");
    }
}

/// The exit status and stderr of `child`, which keeps the child to kill if the wait is
/// abandoned.
pub(super) async fn exited(
    child: &mut Child,
) -> std::io::Result<(std::process::ExitStatus, String)> {
    let mut stderr = Vec::new();
    let mut pipe = child.stderr.take();
    let read = async {
        match pipe.as_mut() {
            Some(pipe) => pipe.read_to_end(&mut stderr).await.map(drop),
            None => Ok(()),
        }
    };
    let (status, read) = tokio::join!(child.wait(), read);
    read?;
    Ok((status?, String::from_utf8_lossy(&stderr).into_owned()))
}
