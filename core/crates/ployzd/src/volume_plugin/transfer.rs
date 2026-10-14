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
use tokio_util::sync::{CancellationToken, DropGuard};

use super::{DockerVolumeName, VolumeStorage};

/// A receive's `kill-daemon` fault fires once this much of the stream is in, so ZFS holds a
/// partial receive to resume.
const KILL_AFTER_BYTES: u64 = 1 << 20;

pub(super) const RECEIVE_STALL: Duration = Duration::from_secs(60);

/// One deadline for all of a departure's stops, well inside Demote's 120 s timeout.
pub(super) const STOP_DEADLINE: Duration = Duration::from_secs(30);

/// The receive tasks this process started, by Volume name. An entry lasts while any of
/// its tasks has a `zfs` child alive, whatever was forgotten or spawned since: that
/// child can still write the slot or hold it busy, and only the entry lets the departure
/// wait for it or name it.
#[derive(Clone, Default)]
pub(super) struct Receives(Arc<Mutex<BTreeMap<String, ReceiveTask>>>);

#[derive(Default)]
struct ReceiveTask {
    /// The latest task's outcome, until it is forgotten or replaced.
    outcome: Arc<Mutex<Option<Result<(), String>>>>,
    lives: Vec<Life>,
}

/// One task's stop signal and its finish, which fires once the task returned and every
/// child it dropped has exited.
struct Life {
    stop: CancellationToken,
    finished: CancellationToken,
}

impl ReceiveTask {
    fn running(&self) -> bool {
        self.lives.iter().any(|life| !life.finished.is_cancelled())
    }
}

/// A receive task's stop signal and a share of its finish. The task has finished once
/// every share is gone, and a `zfs` child dropped unreaped keeps one until it exits.
#[derive(Clone)]
pub(super) struct Stop {
    token: CancellationToken,
    finish: Arc<DropGuard>,
}

impl Stop {
    /// A stop no receive task waits on.
    #[cfg(test)]
    pub(super) fn unowned() -> Self {
        Self {
            token: CancellationToken::new(),
            finish: Arc::new(CancellationToken::new().drop_guard()),
        }
    }

    pub(super) async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

impl Receives {
    /// Whether a task of `name` has not finished, including one that answered but still
    /// waits for a child it dropped to exit.
    pub(super) fn running(&self, name: &DockerVolumeName) -> bool {
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .get(&name.0)
            .is_some_and(ReceiveTask::running)
    }

    pub(super) fn failure(&self, name: &DockerVolumeName) -> Option<String> {
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .get(&name.0)
            .and_then(|task| task.outcome.lock().expect("outcome lock").clone())
            .and_then(Result::err)
    }

    /// Drops `name`'s outcome, and its entry once no task of it has a child alive.
    pub(super) fn forget(&self, name: &DockerVolumeName) {
        let mut tasks = self.0.lock().expect("receive registry is not poisoned");
        let Some(task) = tasks.get_mut(&name.0) else {
            return;
        };
        if task.running() {
            task.outcome = Arc::default();
        } else {
            tasks.remove(&name.0);
        }
    }

    /// Runs `work` as the receive task of `name`. `work` gets the [`Stop`]
    /// [`Self::stop_all`] fires; once it does, `work` kills and reaps its `zfs` children
    /// before returning. A panic is recorded as the task's failure.
    pub(super) fn spawn<F>(&self, name: &DockerVolumeName, work: impl FnOnce(Stop) -> F)
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let life = Life {
            stop: CancellationToken::new(),
            finished: CancellationToken::new(),
        };
        let stop = Stop {
            token: life.stop.clone(),
            finish: Arc::new(life.finished.clone().drop_guard()),
        };
        let held = stop.clone();
        let work = work(stop);
        let outcome = {
            let mut tasks = self.0.lock().expect("receive registry is not poisoned");
            let task = tasks.entry(name.0.clone()).or_default();
            task.lives.retain(|life| !life.finished.is_cancelled());
            task.lives.push(life);
            task.outcome = Arc::default();
            Arc::clone(&task.outcome)
        };
        tokio::spawn(async move {
            let _held = held;
            let result = std::panic::AssertUnwindSafe(work)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("the receive task panicked".to_owned()));
            *outcome.lock().expect("outcome lock") = Some(result);
        });
    }

    /// Stops every receive task and waits until each ended and its `zfs` children
    /// exited, so ZFS holds no slot busy and no receive changes a slot afterwards. Answers
    /// the names with a task still running once `deadline` passes.
    pub(super) async fn stop_all(&self, deadline: Duration) -> Vec<String> {
        let finished: Vec<(String, Vec<CancellationToken>)> = self
            .0
            .lock()
            .expect("receive registry is not poisoned")
            .iter()
            .map(|(name, task)| {
                let finished = task
                    .lives
                    .iter()
                    .map(|life| {
                        life.stop.cancel();
                        life.finished.clone()
                    })
                    .collect();
                (name.clone(), finished)
            })
            .collect();
        let deadline = tokio::time::Instant::now() + deadline;
        let mut stuck = Vec::new();
        for (name, finished) in finished {
            for finished in finished {
                if tokio::time::timeout_at(deadline, finished.cancelled())
                    .await
                    .is_err()
                {
                    stuck.push(name);
                    break;
                }
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
        stop: &Stop,
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
        stop: &Stop,
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
        stop: &Stop,
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
            stop,
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
    finish: Arc<DropGuard>,
    #[cfg(test)]
    spared: bool,
    #[cfg(test)]
    kill_fails: bool,
}

impl ZfsChild {
    /// Spawns a child of the receive task `stop` belongs to, which finishes only once the
    /// child has exited.
    pub(super) fn spawn(command: &mut Command, stop: &Stop) -> std::io::Result<Self> {
        command.spawn().map(|child| Self {
            child: Some(child),
            finish: Arc::clone(&stop.finish),
            #[cfg(test)]
            spared: false,
            #[cfg(test)]
            kill_fails: false,
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

    /// Makes [`Self::reap`]'s kill fail, as one refused by the kernel does.
    #[cfg(test)]
    pub(super) fn fail_kill(&mut self) {
        self.kill_fails = true;
    }

    /// Kills the process unless it already exited, and waits for it. A failed kill still
    /// waits, holding the task's finish, so the departure names a child it cannot stop.
    pub(super) async fn reap(mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Err(error) = self.start_kill(&mut child) {
            tracing::warn!(%error, "could not kill a zfs process");
        }
        if let Err(error) = child.wait().await {
            tracing::warn!(%error, "could not wait for a killed zfs process");
        }
    }

    fn start_kill(&self, child: &mut Child) -> std::io::Result<()> {
        #[cfg(test)]
        if self.kill_fails {
            return Err(std::io::Error::other("injected kill failure"));
        }
        match child.start_kill() {
            // Already waited for.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            result => result,
        }
    }
}

/// Only an early return or a panic gets here; every other path reaps. Dropping cannot
/// await, so a reaper waits for the exit and holds the task's finish until then.
impl Drop for ZfsChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(test)]
        let spared = self.spared;
        #[cfg(not(test))]
        let spared = false;
        if !spared && let Err(error) = child.start_kill() {
            tracing::warn!(%error, "could not kill a zfs process");
        }
        let finish = Arc::clone(&self.finish);
        tokio::spawn(async move {
            let _finish = finish;
            if let Err(error) = child.wait().await {
                tracing::warn!(%error, "could not wait for a killed zfs process");
            }
        });
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
