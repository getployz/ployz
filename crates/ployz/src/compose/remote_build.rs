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
    if let Err(error) = remote::validate_capture(inputs.root(), &definition) {
        return failed(Stage::Preparation, error.to_string()).with_work(evidence);
    }
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
                    Ok(Event::Finished(outcome)) => return validate_outcome(outcome, machine_id, expected, output, evidence),
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
                    if send_cancellation(&sender).await.is_err() {
                        return unknown(stage, "Build cancellation could not be delivered; termination was not confirmed").with_work(evidence);
                    }
                }
            },
            () = cancellation.cancelled(), if !cancelling => {
                cancelling = true;
                deadline = tokio::time::Instant::now() + Duration::from_secs(70);
                // Stop the producer first so cancellation cannot sit behind an
                // unbounded upload. Only two bounded frames can be in flight.
                if send_cancellation(&sender).await.is_err() {
                    return unknown(stage, "Build cancellation could not be delivered; termination was not confirmed").with_work(evidence);
                }
            },
            () = tokio::time::sleep_until(deadline) => return unknown(stage, "Build response deadline expired; termination was not confirmed").with_work(evidence),
        }
    }
}

async fn send_cancellation(
    sender: &mpsc::Sender<ployz_core::OpaquePayload>,
) -> Result<(), remote::InputError> {
    let frame = remote::encode(&Input::Cancel)?;
    tokio::time::timeout(Duration::from_secs(5), sender.send(frame))
        .await
        .map_err(|_| "Build cancellation delivery timed out")?
        .map_err(|_| "Build request stream closed before cancellation".into())
}

fn validate_outcome(
    mut outcome: Outcome,
    expected: MachineId,
    count: usize,
    output: ployz_build::Output,
    evidence: ployz_build::WorkEvidence,
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
                    || !linux_platform(&image.platform)
            }) {
                return unknown(
                    Stage::Output,
                    "Build returned an invalid image identity or platform",
                )
                .with_work(evidence);
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
            )
            .with_work(evidence);
        }
    }
    if let Outcome::Failed { work, .. } | Outcome::Unknown { work, .. } = &mut outcome {
        for (name, observed) in evidence.0 {
            work.0.entry(name).or_insert(observed);
        }
    }
    outcome
}

fn linux_platform(platform: &str) -> bool {
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    let mut parts = platform.split('/');
    parts.next() == Some("linux")
        && parts.next().is_some_and(valid)
        && parts.next().is_none_or(valid)
        && parts.next().is_none()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_validation_keeps_observed_work_and_accepts_linux_cross_builds() {
        use ployz_build::{BuiltImage, Output, TargetEvidence, WorkEvidence};
        let machine_id = MachineId::random();
        let image = BuiltImage {
            reference: format!("example.test/api@sha256:{}", "1".repeat(64)),
            tags: vec!["example.test/api:built".into()],
            platform: "linux/arm/v7".into(),
        };
        let work = WorkEvidence(std::collections::BTreeMap::from([(
            "api".into(),
            TargetEvidence::Image(image.clone()),
        )]));
        for outcome in [
            Outcome::Images {
                machine_id: MachineId::random(),
                images: vec![image.clone()],
            },
            Outcome::Images {
                machine_id,
                images: Vec::new(),
            },
            Outcome::Validated { machine_id },
            Outcome::Images {
                machine_id,
                images: vec![BuiltImage {
                    platform: "linux//v7".into(),
                    ..image.clone()
                }],
            },
        ] {
            let Outcome::Unknown { work: retained, .. } =
                validate_outcome(outcome, machine_id, 1, Output::Load, work.clone())
            else {
                panic!("malformed success was accepted")
            };
            assert_eq!(retained.0, work.0);
        }
        assert!(matches!(
            validate_outcome(
                Outcome::Images {
                    machine_id,
                    images: vec![image]
                },
                machine_id,
                1,
                Output::Load,
                work
            ),
            Outcome::Images { .. }
        ));
    }

    #[tokio::test]
    async fn cancellation_waits_for_a_full_upload_channel() {
        let (sender, mut receiver) = mpsc::channel(2);
        for _ in 0..2 {
            sender
                .send(remote::encode(&Input::Data(vec![1])).unwrap())
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            let (sent, ()) = tokio::join!(send_cancellation(&sender), async {
                tokio::task::yield_now().await;
                for _ in 0..2 {
                    receiver.recv().await.unwrap();
                }
                let frame = receiver.recv().await.unwrap();
                assert!(matches!(
                    remote::decode::<Input>(&frame).unwrap(),
                    Input::Cancel
                ));
            });
            sent.unwrap();
        })
        .await
        .expect("cancellation was dropped behind queued upload frames");
        drop(receiver);
        assert!(send_cancellation(&sender).await.is_err());
    }
}
