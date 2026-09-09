//! Admission and lifetime of one connection-scoped remote Build.

mod queue;
pub(crate) use queue::Runner;

use crate::logs::RpcStream;
use ployz_build::{
    Admission, BuildError, HostPolicy, Output, Progress, Stage,
    remote::{self, Definition, Event, Input, Outcome},
};
use ployz_core::{MachineId, OpaquePayload};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::mpsc;
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tonic::Status;

#[derive(Default)]
struct AttemptState {
    evidence: Mutex<ployz_build::WorkEvidence>,
    retained: Mutex<Option<ployz_build::ImageRetention>>,
}

pub(crate) fn start(
    machine_id: MachineId,
    requests: impl Stream<Item = Result<OpaquePayload, Status>> + Send + Unpin + 'static,
    runner: std::sync::Arc<Runner>,
) -> RpcStream {
    let (events, receiver) = mpsc::channel(8);
    tokio::spawn(async move {
        let shutdown = runner.shutdown.clone();
        let state = Arc::new(AttemptState::default());
        let outcome = attempt(machine_id, requests, &events, runner, state.clone()).await;
        let message = "Build terminal report exceeds the response size limit";
        let fallback = match &outcome {
            Outcome::Unknown { stage, .. } => Outcome::Unknown {
                stage: *stage,
                message: message.into(),
                work: Default::default(),
            },
            Outcome::Failed { stage, .. } => failed(*stage, message),
            Outcome::Images { .. } | Outcome::Validated { .. } | Outcome::Published { .. } => {
                failed(Stage::Output, format!("Build completed; {message}"))
            }
        };
        let payload = remote::encode(&Event::Finished(outcome)).unwrap_or_else(|_| {
            remote::encode(&Event::Finished(fallback)).expect("bounded terminal fallback")
        });
        let _ = tokio::time::timeout(Duration::from_secs(5), events.send(Ok(payload))).await;
        // Admission is already released. Only image retention follows this stream.
        if state
            .retained
            .lock()
            .expect("Build retention lock")
            .is_some()
        {
            tokio::select! { () = events.closed() => {}, () = shutdown.cancelled() => {} }
        }
        let retained = state.retained.lock().expect("Build retention lock").take();
        let _ = tokio::task::spawn_blocking(move || drop(retained)).await;
    });
    ReceiverStream::new(receiver)
}

async fn attempt(
    machine_id: MachineId,
    mut requests: impl Stream<Item = Result<OpaquePayload, Status>> + Send + Unpin + 'static,
    events: &mpsc::Sender<Result<OpaquePayload, Status>>,
    runner: Arc<Runner>,
    state: Arc<AttemptState>,
) -> Outcome {
    let start = tokio::time::timeout(Duration::from_secs(10), requests.next()).await;
    let definition = match start {
        Ok(Some(Ok(payload))) => match remote::decode(&payload) {
            Ok(Input::Start(definition)) => definition,
            _ => return failed(Stage::Admission, "expected a valid Build start frame"),
        },
        _ => return failed(Stage::Admission, "Build request ended before admission"),
    };
    if definition.targets.is_empty()
        || definition.targets.len() > 128
        || definition.image_contexts.len() > 128
    {
        return failed(
            Stage::Admission,
            "Build must name between one and 128 targets",
        );
    }
    let work = ployz_build::WorkEvidence::new(&definition.targets);
    let permit = match runner.enter() {
        Ok(queue::Entry::Active(permit)) => permit,
        Ok(queue::Entry::Waiting(waiting)) => {
            let queued = remote::encode(&Event::Progress(Progress::Stage(Stage::Queued)))
                .expect("bounded queue frame");
            if events.send(Ok(queued)).await.is_err() {
                return failed(Stage::Queued, "Build client disconnected").with_work(work);
            }
            tokio::select! {
                biased;
                () = runner.shutdown.cancelled() => return failed(Stage::Queued, "Build daemon stopped; execution was not attempted").with_work(work),
                () = events.closed() => return failed(Stage::Queued, "Build client disconnected").with_work(work),
                frame = requests.next() => {
                    let reason = match frame {
                        Some(Ok(payload)) if matches!(remote::decode::<Input>(&payload), Ok(Input::Cancel)) => "Build cancelled while queued; execution was not attempted",
                        None | Some(Err(_)) => "Build client disconnected while queued; execution was not attempted",
                        Some(Ok(_)) => "Build upload before admission is forbidden; execution was not attempted",
                    };
                    return failed(Stage::Queued, reason).with_work(work);
                }
                admitted = runner.admit(waiting) => match admitted {
                    Ok(permit) => permit,
                    Err(reason) => return failed(Stage::Queued, reason.to_string()).with_work(work),
                },
            }
        }
        Err(reason) => return failed(Stage::Queued, reason.to_string()).with_work(work),
    };
    let policy = runner.policy.clone();
    let admission = match tokio::task::spawn_blocking({
        let policy = policy.clone();
        move || Admission::try_acquire_with(&policy)
    })
    .await
    {
        Ok(Ok(admission)) => admission,
        Ok(Err(error)) => return failure(Stage::Admission, error).with_work(work),
        Err(_) => return failed(Stage::Admission, "Build admission task failed"),
    };
    *state.evidence.lock().expect("Build evidence lock") =
        ployz_build::WorkEvidence::new(&definition.targets);
    let observed = state.clone();
    let cancellation = admission.cancellation();
    let remaining = admission.remaining();
    let deadline = tokio::time::Instant::now() + remaining;
    let admitted = remote::encode(&Event::Admitted {
        machine_id,
        active_timeout: policy.active_timeout,
    })
    .expect("bounded admission frame");
    if events.send(Ok(admitted)).await.is_err() {
        return failed(Stage::Admission, "Build client disconnected");
    }
    let (upload, source) = mpsc::channel(2);
    let output = events.clone();
    let mut execution = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        receive_and_execute(
            machine_id, definition, source, admission, policy, &output, &observed,
        )
    });
    // Dropping the input pump closes the upload channel. A receiver blocked
    // waiting for another file then wakes and can release unused admission.
    let reason = {
        let input = pump(&mut requests, upload);
        tokio::pin!(input);
        tokio::select! {
            result = &mut execution => return joined(result).with_work(state.evidence.lock().expect("Build evidence lock").clone()),
            reason = &mut input => reason,
            () = runner.shutdown.cancelled() => "Build daemon stopped".to_owned(),
            () = events.closed() => "Build client disconnected".to_owned(),
            () = tokio::time::sleep_until(deadline) => "Build active timeout expired".to_owned(),
        }
    };
    cancellation.cancel();
    // Termination and cleanup have a bounded grace period. A task that cannot
    // confirm its end keeps both its admission and persistent quarantine.
    let outcome = match tokio::time::timeout(Duration::from_secs(70), execution).await {
        Ok(result) => match joined(result) {
            outcome @ Outcome::Unknown { .. } => outcome,
            Outcome::Images { .. } | Outcome::Validated { .. } | Outcome::Published { .. } => {
                failed(
                    Stage::Cleanup,
                    format!("{reason}; output completed before cancellation was observed"),
                )
            }
            Outcome::Failed {
                stage,
                message,
                work,
            } => failed(stage, format!("{reason}; {message}")).with_work(work),
        },
        Err(_) => Outcome::Unknown {
            work: Default::default(),
            stage: Stage::Cleanup,
            message: format!(
                "{reason}; termination was not confirmed; builder ownership is retained"
            ),
        },
    };
    outcome.with_work(state.evidence.lock().expect("Build evidence lock").clone())
}

async fn pump(
    requests: &mut (impl Stream<Item = Result<OpaquePayload, Status>> + Unpin),
    upload: mpsc::Sender<OpaquePayload>,
) -> String {
    let mut finished = false;
    while let Some(payload) = requests.next().await {
        let payload = match payload {
            Ok(payload) => payload,
            Err(_) => return "Build client disconnected".into(),
        };
        match remote::decode::<Input>(&payload) {
            Ok(Input::Cancel) => return "Build cancellation requested".into(),
            Ok(Input::Finish) if !finished => finished = true,
            Ok(_) if !finished => {}
            _ => return "invalid or unexpected Build upload frame".into(),
        }
        if upload.send(payload).await.is_err() {
            return "Build receiver stopped".into();
        }
    }
    "Build client disconnected".into()
}

fn receive_and_execute(
    machine_id: MachineId,
    mut definition: Definition,
    mut source: mpsc::Receiver<OpaquePayload>,
    admission: Admission,
    policy: HostPolicy,
    events: &mpsc::Sender<Result<OpaquePayload, Status>>,
    state: &AttemptState,
) -> Outcome {
    let cancellation = admission.cancellation();
    let deadline = std::time::Instant::now() + admission.remaining();
    let mut upload = match admission.upload() {
        Ok(upload) => upload,
        Err(error) => return failed(Stage::Upload, error.to_string()),
    };
    loop {
        let Some(payload) = source.blocking_recv() else {
            return failed(
                Stage::Upload,
                "Build upload was interrupted; execution was not attempted",
            );
        };
        let frame = match remote::decode::<Input>(&payload) {
            Ok(frame) => frame,
            Err(error) => return failed(Stage::Upload, error.to_string()),
        };
        let finished = matches!(frame, Input::Finish);
        if let Err(error) = upload.accept(frame) {
            return failed(Stage::Upload, error.to_string());
        }
        if finished {
            break;
        }
    }
    let progress = |progress| {
        state
            .evidence
            .lock()
            .expect("Build evidence lock")
            .observe(&progress);
        let Ok(mut frame) = remote::encode(&Event::Progress(progress)).map(Ok) else {
            return;
        };
        loop {
            match events.try_send(frame) {
                Ok(()) => return,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    cancellation.cancel();
                    return;
                }
                Err(mpsc::error::TrySendError::Full(value)) => frame = value,
            }
            if cancellation.is_cancelled() || std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    progress(Progress::Stage(Stage::Upload));
    // Keep bytes on the Machines. Reuse the Docker peer-pull bridge because
    // Buildx cannot reliably encode IPv6 registry keys in its TOML config.
    let mut proxies = Vec::new();
    for context in definition.image_contexts.values_mut() {
        let proxy = match tokio::runtime::Handle::current()
            .block_on(crate::docker::ImageProxy::open(context.source))
        {
            Ok(proxy) => proxy,
            Err(error) => return failed(Stage::Preparation, error.to_string()),
        };
        context.source.management_address =
            ployz_core::ManagementAddress(std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped());
        context.source.port = proxy.port;
        proxies.push(proxy);
    }
    let result = upload.execute(&definition, Some(&policy.docker), &progress);
    match result {
        Ok(completed) => {
            if !definition.retained_tags.is_empty() {
                *state.retained.lock().expect("Build retention lock") = Some(completed.retention);
            }
            match definition.output {
                Output::Load => Outcome::Images {
                    machine_id,
                    images: completed.images,
                },
                Output::Validate => Outcome::Validated { machine_id },
                Output::Registry => Outcome::Published { machine_id },
            }
        }
        Err(error) => failure(error.stage(), error),
    }
}

fn joined(result: Result<Outcome, tokio::task::JoinError>) -> Outcome {
    result.unwrap_or_else(|_| Outcome::Unknown {
        work: Default::default(),
        stage: Stage::Cleanup,
        message: "Build task failed; termination cannot be confirmed".into(),
    })
}
fn failed(stage: Stage, message: impl Into<String>) -> Outcome {
    Outcome::Failed {
        work: Default::default(),
        stage,
        message: message.into(),
    }
}
fn failure(stage: Stage, error: BuildError) -> Outcome {
    if error.is_unknown() {
        Outcome::Unknown {
            work: Default::default(),
            stage,
            message: error.to_string(),
        }
    } else {
        failed(stage, error.to_string())
    }
}

#[cfg(test)]
mod tests;
