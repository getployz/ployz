//! The receive task: one stream from the writer Machine into the slot's `fs`, identified
//! by the position that started it so a restarted plugin still answers for it.

use std::{
    collections::BTreeMap,
    fmt,
    process::Stdio,
    str::FromStr,
    sync::{Arc, Mutex},
};

use axum::{Json, extract::State};
use futures_util::StreamExt as _;
use ployz_core::{
    InspectReceiveRequest, Lease, ManagementAddress, Pos, ReceiveStatus, ReceiveView, RpcError,
    SendSource, SendStream, SnapshotName, StartReceiveRequest, SwitchError, SwitchReply,
};
use tokio::{io::AsyncWriteExt as _, process::Command};

use super::{
    Dataset, DockerVolumeName, VolumeStorage,
    lease::{RESUME_TOKEN_PROPERTY, internal, slot_fs},
    mirror::name,
};

/// On the slot parent: `<lease>:<seq>.<round>.<sub>:<target>` of the receive last started.
pub(super) const RECEIVE_PROPERTY: &str = "ployz:receive";

/// `kill-daemon:StartReceive` fires once this much of the stream is in, so ZFS holds a
/// partial receive to resume.
const KILL_AFTER_BYTES: u64 = 1 << 20;

/// Which receive a slot last admitted. Outlives the plugin process, unlike the task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReceiveRecord {
    pub(super) lease: Lease,
    pub(super) pos: Pos,
    pub(super) target: SnapshotName,
}

impl fmt::Display for ReceiveRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}:{}", self.lease, self.pos, self.target)
    }
}

impl FromStr for ReceiveRecord {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid receive record {value:?}");
        let mut fields = value.split(':');
        let (Some(lease), Some(pos), Some(target), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid());
        };
        let mut parts = pos.split('.');
        let (Some(seq), Some(round), Some(sub), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        Ok(Self {
            lease: Lease::new(lease.parse().map_err(|_| invalid())?),
            pos: Pos {
                seq: seq.parse().map_err(|_| invalid())?,
                round: round.parse().map_err(|_| invalid())?,
                sub: sub.parse().map_err(|_| invalid())?,
            },
            target: target.parse().map_err(|_| invalid())?,
        })
    }
}

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

    fn failure(&self, name: &DockerVolumeName) -> Option<String> {
        self.0
            .lock()
            .expect("receive registry is not poisoned")
            .get(&name.0)
            .and_then(|task| task.outcome.lock().expect("outcome lock").clone())
            .and_then(Result::err)
    }

    fn start(&self, name: &DockerVolumeName) -> Arc<Mutex<Option<Result<(), String>>>> {
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

impl VolumeStorage {
    pub(super) async fn receive_record(
        &self,
        slot: &Dataset,
    ) -> Result<Option<ReceiveRecord>, RpcError> {
        match self
            .property(&slot.name, RECEIVE_PROPERTY)
            .await
            .map_err(internal)?
        {
            None => Ok(None),
            Some(value) => value
                .parse()
                .map(Some)
                .map_err(|error: String| internal(format!("{error} on {}", slot.name).into())),
        }
    }

    async fn start_receive(&self, request: &StartReceiveRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let slot = scope.require_slot(&name)?;
        let (slot, bound) = (slot.name.clone(), slot.refquota);
        self.require_no_receive(&name)?;
        let fs = slot_fs(&scope.pool, &name);
        // The plugin can die after recording and before the receive creates `fs`, so a
        // replay looks for the landed target in ZFS and otherwise starts the receive again.
        if scope.replayed()
            && let Some(received) = scope.fs(&name)
            && let Some(newest) = self
                .snapshots(&received.name)
                .await
                .map_err(internal)?
                .first()
            && newest.name == request.target
        {
            return self.reply(&scope.pool, &name, scope.admitted).await;
        }
        if bound == 0 {
            return Err(
                SwitchError::Precondition.rpc_error(format!("mirror slot {slot} carries no bound"))
            );
        }
        let record = ReceiveRecord {
            lease: request.switch.lease,
            pos: request.switch.pos,
            target: request.target.clone(),
        };
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("StartReceive");
        self.zfs(&["set", &format!("{RECEIVE_PROPERTY}={record}"), &slot])
            .await
            .map_err(internal)?;
        let source = match (&request.resume_token, request.base) {
            (Some(token), _) => SendSource::Resume {
                token: token.clone(),
            },
            (None, Some(base)) => SendSource::Incremental {
                base,
                target: request.target.clone(),
            },
            (None, None) => SendSource::Full {
                target: request.target.clone(),
            },
        };
        let stream = SendStream {
            name: request.name.clone(),
            source,
        };
        let outcome = self.receives.start(&name);
        let storage = self.clone();
        let from = request.from;
        tokio::spawn(async move {
            let result = storage.receive(from, &stream, &fs, bound).await;
            if let Err(error) = &result {
                tracing::warn!(%fs, error, "Volume receive failed");
            }
            *outcome.lock().expect("outcome lock") = Some(result);
        });
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    /// Pulls one stream into `fs`, then re-asserts and verifies the slot's bound and
    /// read-only state, which ZFS drops on an interrupted first receive.
    async fn receive(
        &self,
        from: ManagementAddress,
        stream: &SendStream,
        fs: &str,
        bound: u64,
    ) -> Result<(), String> {
        let url = format!(
            "http://[{}]:{}{}",
            from.0,
            self.send_port,
            stream.path_and_query()
        );
        let response = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| format!("GET {url}: {error}"))?;
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
            .spawn()
            .map_err(|error| format!("could not run zfs receive: {error}"))?;
        let mut stdin = child.stdin.take().expect("zfs receive stdin is piped");
        let mut body = response.bytes_stream();
        let mut received = 0u64;
        let mut broke = None;
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => {
                    if stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                    let before = received;
                    received += bytes.len() as u64;
                    if before < KILL_AFTER_BYTES && received >= KILL_AFTER_BYTES {
                        ployzd::faults::kill_inside("StartReceive");
                    }
                }
                Err(error) => {
                    broke = Some(error.to_string());
                    break;
                }
            }
        }
        drop(stdin);
        let output = child
            .wait_with_output()
            .await
            .map_err(|error| format!("zfs receive did not finish: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "zfs receive into {fs} failed after {received} bytes: {}{}",
                stderr.trim(),
                broke.map_or(String::new(), |error| format!(" (stream: {error})"))
            ));
        }
        if let Some(error) = broke {
            return Err(format!(
                "stream from {url} broke after {received} bytes: {error}"
            ));
        }
        for property in ["readonly=on", &refquota] {
            self.zfs(&["set", property, fs])
                .await
                .map_err(|error| error.to_string())?;
        }
        self.verify_copy(fs, bound).await
    }

    async fn inspect_receive(
        &self,
        request: &InspectReceiveRequest,
    ) -> Result<ReceiveView, RpcError> {
        let name = name(&request.name)?;
        let idle = Ok(ReceiveView {
            round: None,
            status: ReceiveStatus::Idle,
        });
        let _guard = self.mutation.lock().await;
        let Some(pool) = self.pool.one_usable().await.map_err(internal)? else {
            return idle;
        };
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let Some(slot) = Self::slot(&datasets, &pool, &name) else {
            return idle;
        };
        let Some(record) = self.receive_record(slot).await? else {
            return idle;
        };
        let round = Some(record.pos.round);
        if record.pos.round != request.round {
            return Ok(ReceiveView {
                round,
                status: ReceiveStatus::Idle,
            });
        }
        let target = record.target;
        if self.receives.running(&name) {
            return Ok(ReceiveView {
                round,
                status: ReceiveStatus::Running { target },
            });
        }
        let fs = Self::slot_fs(&datasets, &pool, &name);
        if let Some(fs) = fs
            && let Some(token) = self
                .property(&fs.name, RESUME_TOKEN_PROPERTY)
                .await
                .map_err(internal)?
        {
            return Ok(ReceiveView {
                round,
                status: ReceiveStatus::Resumable { target, token },
            });
        }
        if let Some(reason) = self.receives.failure(&name) {
            return Ok(ReceiveView {
                round,
                status: ReceiveStatus::Failed { target, reason },
            });
        }
        if let Some(fs) = fs
            && let Some(newest) = self.snapshots(&fs.name).await.map_err(internal)?.first()
            && newest.name == target
        {
            // The task that landed it may have died before its own check ran.
            return Ok(ReceiveView {
                round,
                status: match self.verify_copy(&fs.name, slot.refquota).await {
                    Ok(()) => ReceiveStatus::Done {
                        newest: newest.clone(),
                    },
                    Err(reason) => ReceiveStatus::Failed { target, reason },
                },
            });
        }
        Ok(ReceiveView {
            round,
            status: ReceiveStatus::Failed {
                target,
                reason: "the receive did not complete".to_owned(),
            },
        })
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

pub(super) async fn start_receive(
    State(storage): State<VolumeStorage>,
    Json(request): Json<StartReceiveRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.start_receive(&request).await)
}

pub(super) async fn inspect_receive(
    State(storage): State<VolumeStorage>,
    Json(request): Json<InspectReceiveRequest>,
) -> Json<Result<ReceiveView, RpcError>> {
    Json(storage.inspect_receive(&request).await)
}
