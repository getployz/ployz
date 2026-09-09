//! Admission and lifetime of one connection-scoped remote Build.

use crate::logs::RpcStream;
use ployz_build::{
    Admission, BuildError, HostPolicy, Output, Progress, Stage,
    remote::{self, Definition, Event, Input, Outcome, Upload},
};
use ployz_core::{MachineId, OpaquePayload};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tonic::Status;

pub(crate) fn start(
    machine_id: MachineId,
    requests: impl Stream<Item = Result<OpaquePayload, Status>> + Send + Unpin + 'static,
    policy: HostPolicy,
) -> RpcStream {
    let (events, receiver) = mpsc::channel(8);
    tokio::spawn(async move {
        let outcome = attempt(machine_id, requests, &events, policy).await;
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
    });
    ReceiverStream::new(receiver)
}

async fn attempt(
    machine_id: MachineId,
    mut requests: impl Stream<Item = Result<OpaquePayload, Status>> + Send + Unpin + 'static,
    events: &mpsc::Sender<Result<OpaquePayload, Status>>,
    policy: HostPolicy,
) -> Outcome {
    let start = tokio::time::timeout(Duration::from_secs(10), requests.next()).await;
    let definition = match start {
        Ok(Some(Ok(payload))) => match remote::decode(&payload) {
            Ok(Input::Start(definition)) => definition,
            _ => return failed(Stage::Admission, "expected a valid Build start frame"),
        },
        _ => return failed(Stage::Admission, "Build request ended before admission"),
    };
    if definition.targets.is_empty() || definition.targets.len() > 128 {
        return failed(
            Stage::Admission,
            "Build must name between one and 128 targets",
        );
    }
    let admission = match tokio::task::spawn_blocking({
        let policy = policy.clone();
        move || Admission::try_acquire_with(&policy)
    })
    .await
    {
        Ok(Ok(admission)) => admission,
        Ok(Err(error)) => return failure(Stage::Admission, error),
        Err(_) => return failed(Stage::Admission, "Build admission task failed"),
    };
    let evidence = std::sync::Arc::new(std::sync::Mutex::new(ployz_build::WorkEvidence::new(
        &definition.targets,
    )));
    let observed = evidence.clone();
    let cancellation = admission.cancellation();
    let remaining = admission.remaining();
    let deadline = tokio::time::Instant::now() + remaining;
    let admitted =
        remote::encode(&Event::Admitted { machine_id }).expect("bounded admission frame");
    if events.send(Ok(admitted)).await.is_err() {
        return failed(Stage::Admission, "Build client disconnected");
    }
    let (upload, source) = mpsc::channel(2);
    let output = events.clone();
    let mut execution = tokio::task::spawn_blocking(move || {
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
            result = &mut execution => return joined(result).with_work(evidence.lock().expect("Build evidence lock").clone()),
            reason = &mut input => reason,
            () = events.closed() => "Build client disconnected".to_owned(),
            () = tokio::time::sleep_until(deadline) => "Build active timeout expired".to_owned(),
        }
    };
    cancellation.cancel();
    // Termination and cleanup have a bounded grace period. A task that cannot
    // confirm its end keeps both its admission and persistent quarantine.
    let outcome = match tokio::time::timeout(Duration::from_secs(70), execution).await {
        Ok(result) => match joined(result) {
            outcome @ (Outcome::Images { .. }
            | Outcome::Validated { .. }
            | Outcome::Published { .. }
            | Outcome::Unknown { .. }) => outcome,
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
    outcome.with_work(evidence.lock().expect("Build evidence lock").clone())
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
    definition: Definition,
    mut source: mpsc::Receiver<OpaquePayload>,
    admission: Admission,
    policy: HostPolicy,
    events: &mpsc::Sender<Result<OpaquePayload, Status>>,
    evidence: &std::sync::Mutex<ployz_build::WorkEvidence>,
) -> Outcome {
    let cancellation = admission.cancellation();
    let mut upload = match Upload::new() {
        Ok(upload) => upload,
        Err(error) => return failed(Stage::Upload, error.to_string()),
    };
    loop {
        if let Err(error) = admission.check() {
            return failure(Stage::Upload, error);
        }
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
    let upload = match upload.complete() {
        Ok(upload) => upload,
        Err(error) => return failed(Stage::Upload, error.to_string()),
    };
    let deadline = std::time::Instant::now() + admission.remaining();
    let progress = |progress| {
        evidence
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
    match upload.execute(&definition, admission, Some(&policy.docker), &progress) {
        Ok(images) => match definition.output {
            Output::Load => Outcome::Images { machine_id, images },
            Output::Validate => Outcome::Validated { machine_id },
            Output::Registry => Outcome::Published { machine_id },
        },
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
