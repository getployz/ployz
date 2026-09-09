//! The client-side adapter: one captured Build, one already-selected Machine,
//! one submission. It never opens Docker or inspects a local image.

use super::build_inputs::BuildInputs;
use crate::connect::Client;
use ployz_build::{
    Progress, Stage,
    remote::{self, Definition, Event, Input, Outcome},
};
use ployz_core::{MachineId, MachineTarget};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub(super) async fn execute(
    inputs: BuildInputs,
    definition: Definition,
    client: &Client,
    machine_id: MachineId,
    cancellation: CancellationToken,
    progress: impl Fn(Progress),
) -> Outcome {
    let mut evidence = ployz_build::WorkEvidence::new(&definition.targets);
    let expected = definition.targets.len();
    let output = definition.output;
    let (sender, receiver) = mpsc::channel(2);
    let start = match remote::encode(&Input::Start(definition)) {
        Ok(start) => start,
        Err(message) => return failed(Stage::Admission, message.to_string()),
    };
    sender.send(start).await.expect("request receiver is owned");
    let target = MachineTarget::from(&machine_id);
    let mut responses = match tokio::time::timeout(
        Duration::from_secs(10),
        client.build_stream(&target, ReceiverStream::new(receiver)),
    )
    .await
    {
        Ok(Ok(responses)) => responses,
        Ok(Err(error)) => {
            return failed(
                Stage::Admission,
                format!("Machine {machine_id} cannot accept the Build: {error}"),
            );
        }
        Err(_) => {
            return failed(
                Stage::Admission,
                format!("Machine {machine_id} did not answer; source was not uploaded"),
            );
        }
    };
    match tokio::time::timeout(Duration::from_secs(10), responses.message()).await {
        Ok(Ok(Some(payload))) => match remote::decode::<Event>(&payload) {
            Ok(Event::Admitted {
                machine_id: admitted,
            }) if admitted == machine_id => {}
            Ok(Event::Finished(outcome @ (Outcome::Failed { .. } | Outcome::Unknown { .. }))) => {
                return outcome.with_work(evidence);
            }
            _ => {
                return failed(
                    Stage::Admission,
                    "invalid Build admission response; source was not uploaded",
                );
            }
        },
        _ => {
            return failed(
                Stage::Admission,
                "Build admission was not observed; source was not uploaded",
            );
        }
    }
    progress(Progress::Stage(Stage::Upload));
    let stop = cancellation.child_token();
    let _stop_upload = stop.clone().drop_guard();
    let producer = sender.clone();
    let mut upload = tokio::task::spawn_blocking(move || {
        remote::upload(inputs.root(), |frame| {
            let mut payload = remote::encode(&frame)?;
            loop {
                if stop.is_cancelled() {
                    return Err("Build upload cancelled".into());
                }
                match producer.try_send(payload) {
                    Ok(()) => return Ok(()),
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Err("Build upload stream closed".into());
                    }
                    Err(mpsc::error::TrySendError::Full(value)) => payload = value,
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    });
    let mut uploaded = false;
    let mut cancelling = false;
    let mut stage = Stage::Upload;
    let mut deadline =
        tokio::time::Instant::now() + ployz_build::EXECUTION_TIMEOUT + Duration::from_secs(70);
    loop {
        tokio::select! {
            result = responses.message() => match result {
                Ok(Some(payload)) => match remote::decode::<Event>(&payload) {
                    Ok(Event::Progress(event)) => {
                        if let Progress::Stage(observed) = &event { stage = *observed; }
                        evidence.observe(&event);
                        progress(event);
                    }
                    Ok(Event::Finished(outcome)) => return validate_outcome(outcome, machine_id, expected, output),
                    _ => return unknown(stage, "invalid Build response; termination was not confirmed").with_work(evidence),
                },
                _ => return unknown(stage, "Build stream disconnected; termination was not confirmed").with_work(evidence),
            },
            result = &mut upload, if !uploaded => {
                uploaded = true;
                if !matches!(result, Ok(Ok(()))) && !cancelling {
                    cancelling = true;
                    deadline = tokio::time::Instant::now() + Duration::from_secs(70);
                    // The capture cannot be completed; ask the host to release
                    // admission and wait for its typed termination evidence.
                    if let Ok(frame) = remote::encode(&Input::Cancel) { let _ = sender.try_send(frame); }
                }
            },
            () = cancellation.cancelled(), if !cancelling => {
                cancelling = true;
                deadline = tokio::time::Instant::now() + Duration::from_secs(70);
                // Stop the producer first so cancellation cannot sit behind an
                // unbounded upload. Only two bounded frames can be in flight.
                let frame = remote::encode(&Input::Cancel).expect("bounded cancellation frame");
                if tokio::time::timeout(Duration::from_secs(5), sender.send(frame)).await.is_err() {
                    return unknown(stage, "Build cancellation could not be delivered; termination was not confirmed").with_work(evidence);
                }
            },
            () = tokio::time::sleep_until(deadline) => return unknown(stage, "Build response deadline expired; termination was not confirmed").with_work(evidence),
        }
    }
}

fn validate_outcome(
    outcome: Outcome,
    expected: MachineId,
    count: usize,
    output: ployz_build::Output,
) -> Outcome {
    match &outcome {
        Outcome::Images { machine_id, images }
            if *machine_id == expected
                && images.len() == count
                && output == ployz_build::Output::Load =>
        {
            if images.iter().any(|image| {
                image
                    .reference
                    .parse::<oci_client::Reference>()
                    .ok()
                    .and_then(|reference| reference.digest().map(str::to_owned))
                    .is_none()
                    || image.tags.is_empty()
                    || !matches!(
                        image.platform.as_str(),
                        "linux/amd64" | "linux/arm64" | "linux/arm64/v8"
                    )
            }) {
                return unknown(
                    Stage::Output,
                    "Build returned an invalid image identity or platform",
                );
            }
        }
        Outcome::Validated { machine_id }
            if *machine_id == expected && output == ployz_build::Output::Validate => {}
        Outcome::Published { machine_id }
            if *machine_id == expected && output == ployz_build::Output::Registry => {}
        Outcome::Failed { .. } | Outcome::Unknown { .. } => {}
        Outcome::Images { .. } | Outcome::Validated { .. } | Outcome::Published { .. } => {
            return unknown(
                Stage::Output,
                "Build result does not match the admitted request",
            );
        }
    }
    outcome
}
fn failed(stage: Stage, message: impl Into<String>) -> Outcome {
    Outcome::Failed {
        work: Default::default(),
        stage,
        message: message.into(),
    }
}
fn unknown(stage: Stage, message: impl Into<String>) -> Outcome {
    Outcome::Unknown {
        work: Default::default(),
        stage,
        message: message.into(),
    }
}
