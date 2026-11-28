//! The client-side adapter: one captured Build, one already-selected Machine,
//! one submission. It never opens Docker or inspects a local image.

use super::RemoteBuildFailure;
use super::inputs::BuildInputs;
use crate::connect::Client;
use ployz_build::{
    Access, Progress, Stage, TIMING,
    remote::{self, Definition, Event, Input, Outcome},
};
use ployz_core::error_chain;
use ployz_core::{MachineId, MachineTarget, op};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

impl Client {
    /// Check all command targets before selecting this Machine or sending source.
    /// # Errors
    /// Returns target-local refusals, transport failures, or cancellation.
    pub(crate) async fn check_build_capabilities(
        &self,
        machine_id: MachineId,
        targets: &[ployz_build::Target],
        access: Access,
        cancellation: &CancellationToken,
    ) -> Result<(), CapabilityCheckError> {
        let mut admitted = match open_and_admit(
            self,
            Input::Check(targets.to_vec()),
            access,
            machine_id,
            cancellation,
            &mut |_| {},
            &Default::default(),
        )
        .await
        {
            Ok(admitted) => admitted,
            Err(Refused {
                failure,
                by_enforcement,
            }) => {
                let message = failure.into_message();
                return Err(if by_enforcement {
                    CapabilityCheckError::RefusedByEnforcement(message)
                } else {
                    remote::InputError::from(message).into()
                });
            }
        };
        // Keep the admitted sender alive: closing it would cancel the worker check.
        let deadline = tokio::time::Instant::now() + TIMING.result_wait(admitted.active_timeout);
        let unusable =
            |message: &str| CapabilityCheckError::from(remote::InputError::from(message));
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(unusable("Build selection cancelled")),
            () = tokio::time::sleep_until(deadline) => Err(unusable("Build capability deadline expired")),
            response = admitted.responses.message() => {
                let payload = response.map_err(|error| remote::InputError::from(error.to_string()))?
                    .ok_or_else(|| unusable("Build capability response ended early"))?;
                match remote::decode::<Event>(&payload)? {
                    Event::Finished(Outcome::CapabilitiesChecked { machine_id: actual }) if actual == machine_id => Ok(()),
                    Event::Finished(Outcome::Failed { message, .. } | Outcome::Unknown { message, .. }) => Err(remote::InputError::from(message).into()),
                    Event::Admitted { .. } | Event::Progress(_) | Event::Finished(_) => Err(unusable("invalid Build capability response")),
                }
            }
        }
    }
}

pub(super) enum Completion {
    /// Each image, with the receipt its Server gave when a free Build made it.
    Images {
        images: Vec<(ployz_build::BuiltImage, Option<ployz_core::FreeReceipt>)>,
        stream: Arc<Mutex<tonic::Streaming<ployz_core::OpaquePayload>>>,
    },
    /// The Build Machine reported the attempt failed, or its termination unconfirmed.
    Failed(RemoteBuildFailure),
    /// No admission by the caller's deadline; the Build was withdrawn before any source left.
    Withdrawn,
}

pub(super) async fn execute(
    inputs: BuildInputs,
    (definition, access): (Definition, Access),
    client: &Client,
    machine_id: MachineId,
    admit_by: Option<tokio::time::Instant>,
    cancellation: CancellationToken,
    progress: impl Fn(Progress),
) -> Completion {
    let mut queued = None;
    let mut executing = None;
    let outcome = execute_attempt(
        inputs,
        (definition, access),
        client,
        machine_id,
        admit_by,
        cancellation,
        |event| {
            match &event {
                Progress::Stage(Stage::Queued) => {
                    queued.get_or_insert_with(tokio::time::Instant::now);
                }
                Progress::Stage(Stage::Upload) => {
                    executing.get_or_insert_with(tokio::time::Instant::now);
                }
                Progress::Stage(_)
                | Progress::Output(_)
                | Progress::Step(_)
                | Progress::StepOutput { .. }
                | Progress::Timing { .. }
                | Progress::Target { .. } => {}
            }
            progress(event);
        },
    )
    .await;
    let finished = tokio::time::Instant::now();
    progress(Progress::Timing {
        queue_wait: queued.map_or(Duration::ZERO, |start| {
            executing.unwrap_or(finished) - start
        }),
        execution: executing.map_or(Duration::ZERO, |start| finished - start),
    });
    outcome
}

/// A validated admission keeps its request stream open until the continuation ends.
struct Admitted {
    sender: mpsc::Sender<ployz_core::OpaquePayload>,
    responses: tonic::Streaming<ployz_core::OpaquePayload>,
    active_timeout: Duration,
}

struct Refused {
    failure: RemoteBuildFailure,
    by_enforcement: bool,
}

impl From<RemoteBuildFailure> for Refused {
    fn from(failure: RemoteBuildFailure) -> Self {
        Self {
            failure,
            by_enforcement: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CapabilityCheckError {
    #[error("{0}")]
    RefusedByEnforcement(String),
    #[error(transparent)]
    Failed(#[from] remote::InputError),
}

impl CapabilityCheckError {
    /// Whether the Machine refused by Device enforcement. A daemon from before refusals
    /// named their capability sent the sentence as a failure message, and reads by it.
    pub(crate) fn refused_by_enforcement(&self) -> bool {
        matches!(self, Self::RefusedByEnforcement(_))
            || self.to_string() == ployz_core::BUILD_REFUSED_BY_ENFORCEMENT
    }
}

async fn open_and_admit(
    client: &Client,
    request: Input,
    access: Access,
    machine_id: MachineId,
    cancellation: &CancellationToken,
    progress: &mut impl FnMut(Progress),
    evidence: &ployz_build::WorkEvidence,
) -> Result<Admitted, Refused> {
    let (sender, receiver) = mpsc::channel(2);
    let start = match remote::encode(&request) {
        Ok(start) => start,
        Err(message) => return Err(failed(Stage::Admission, message.to_string()).into()),
    };
    sender.send(start).await.expect("request receiver is owned");
    let target = MachineTarget::from(&machine_id);
    let requests = ReceiverStream::new(receiver);
    let capability = match access {
        Access::Full => ployz_core::BUILD_CAPABILITY,
        Access::Free => ployz_core::BUILD_FREE_CAPABILITY,
    };
    let opened = async {
        match access {
            Access::Full => {
                client
                    .open_stream::<op::Build>(Some(&target), requests)
                    .await
            }
            Access::Free => {
                client
                    .open_stream::<op::BuildFree>(Some(&target), requests)
                    .await
            }
        }
    };
    let mut responses = match tokio::time::timeout(TIMING.admission_wait, opened).await {
        Ok(Ok(responses)) => responses,
        Ok(Err(error)) => {
            let refusal = error.to_rpc_error();
            // An enforcing Machine's refusal is a sentence of its own, not a failure to reach it.
            if crate::connect::refused_by_enforcement(&refusal, capability) {
                return Err(Refused {
                    failure: failed(Stage::Admission, refusal.message),
                    by_enforcement: true,
                });
            }
            return Err(failed(
                Stage::Admission,
                format!(
                    "Machine {machine_id} cannot accept the Build: {}",
                    error_chain::row(&error)
                ),
            )
            .into());
        }
        Err(_) => {
            return Err(failed(
                Stage::Admission,
                format!("Machine {machine_id} did not answer; source was not uploaded"),
            )
            .into());
        }
    };
    let invalid_response = || {
        failed(
            Stage::Admission,
            "invalid Build admission response; source was not uploaded",
        )
        .with_work(evidence.clone())
    };
    let waiting_since = tokio::time::Instant::now();
    let mut admission_deadline = waiting_since + TIMING.admission_wait;
    let mut queued = false;
    let active_timeout = loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                // No source has left this client. Dropping the stream removes
                // its connection-scoped waiter, even if admission raced cancellation.
                return Err(failed(Stage::Queued, "Build cancelled before upload; execution was not attempted").with_work(evidence.clone()).into());
            }
            result = responses.message() => match result {
                Ok(Some(payload)) => match remote::decode::<Event>(&payload) {
                    Ok(Event::Admitted { machine_id: admitted, active_timeout })
                        if admitted == machine_id && !active_timeout.is_zero() && active_timeout <= TIMING.max_policy_timeout => break active_timeout,
                    Ok(Event::Progress(Progress::Stage(Stage::Queued))) if !queued => {
                        queued = true;
                        // The Machine's validated local policy is bounded at 24h.
                        admission_deadline = waiting_since + TIMING.queued_admission_wait();
                        progress(Progress::Stage(Stage::Queued));
                    }
                    Ok(Event::Finished(outcome)) => return Err(RemoteBuildFailure::try_from(outcome).map(|failure| failure.with_work(evidence.clone())).unwrap_or_else(|_| invalid_response()).into()),
                    _ => return Err(invalid_response().into()),
                },
                Ok(None) => return Err(failed(Stage::Admission, "Build stream ended before admission; source was not uploaded").with_work(evidence.clone()).into()),
                Err(status) => return Err(failed(Stage::Admission, format!("{}; source was not uploaded", disconnected(status))).with_work(evidence.clone()).into()),
            },
            () = tokio::time::sleep_until(admission_deadline) => return Err(failed(Stage::Queued, "Build admission deadline expired; source was not uploaded").with_work(evidence.clone()).into()),
        }
    };
    Ok(Admitted {
        sender,
        responses,
        active_timeout,
    })
}

async fn execute_attempt(
    inputs: BuildInputs,
    (definition, access): (Definition, Access),
    client: &Client,
    machine_id: MachineId,
    admit_by: Option<tokio::time::Instant>,
    cancellation: CancellationToken,
    mut progress: impl FnMut(Progress),
) -> Completion {
    let mut evidence = ployz_build::WorkEvidence::new(&definition.targets);
    // The Builder opens the Sealed values; here their files are named, not present.
    let files = match access {
        Access::Full => Ok(remote::SealedFiles::Pending),
        Access::Free => inputs
            .without_registry_config()
            .map(|()| remote::SealedFiles::Free),
    };
    if let Err(error) = files.map_err(|error| error.to_string()).and_then(|files| {
        remote::validate_capture(inputs.root(), &definition, files)
            .map_err(|error| error.to_string())
    }) {
        return Completion::Failed(failed(Stage::Preparation, error).with_work(evidence));
    }
    let expected = definition.targets.len();
    let admission = open_and_admit(
        client,
        Input::Start(definition),
        access,
        machine_id,
        &cancellation,
        &mut progress,
        &evidence,
    );
    let admitted = match admit_by {
        None => admission.await,
        Some(deadline) => tokio::select! {
            biased;
            admitted = admission => admitted,
            // Dropping the stream removes its waiter; no source has left this client.
            () = tokio::time::sleep_until(deadline) => return Completion::Withdrawn,
        },
    };
    let Admitted {
        sender,
        mut responses,
        active_timeout,
    } = match admitted {
        Ok(admitted) => admitted,
        Err(Refused { failure, .. }) => return Completion::Failed(failure),
    };
    let mut retained = None;
    let outcome = async {
        progress(Progress::Stage(Stage::Upload));
        let stop = cancellation.child_token();
        let _stop_upload = stop.clone().drop_guard();
        let producer = sender.clone();
        let runtime = tokio::runtime::Handle::current();
        let mut upload = tokio::task::spawn_blocking(move || {
            produce(inputs.root(), &producer, &stop, &runtime)
        });
        let mut uploaded = false;
        let mut cancelling = false;
        let mut stage = Stage::Upload;
        let mut deadline = tokio::time::Instant::now() + TIMING.result_wait(active_timeout);
        loop {
            tokio::select! {
                result = responses.message() => match result {
                    Ok(Some(payload)) => match remote::decode::<Event>(&payload) {
                        Ok(Event::Progress(event)) => {
                            if let Progress::Stage(observed) = &event { stage = *observed; }
                            evidence.observe(&event);
                            progress(event);
                        }
                        Ok(Event::Finished(outcome)) => {
                            let validated = validate_outcome(outcome, (machine_id, access), expected, evidence);
                            if validated.is_ok() { retained = Some(responses); }
                            return validated;
                        },
                        _ => return Err(unknown(stage, "invalid Build response; termination was not confirmed").with_work(evidence)),
                    },
                    Ok(None) => return Err(unknown(stage, "Build stream ended without a result; termination was not confirmed").with_work(evidence)),
                    Err(status) => return Err(unknown(stage, format!("{}; termination was not confirmed", disconnected(status))).with_work(evidence)),
                },
                result = &mut upload, if !uploaded => {
                    uploaded = true;
                    if !matches!(result, Ok(Ok(()))) && !cancelling {
                        cancelling = true;
                        deadline = tokio::time::Instant::now() + TIMING.termination_grace;
                        // The capture cannot be completed; ask the host to release
                        // admission and wait for its typed termination evidence.
                        if send_cancellation(&sender).await.is_err() {
                            return Err(unknown(stage, "Build cancellation could not be delivered; termination was not confirmed").with_work(evidence));
                        }
                    }
                },
                () = cancellation.cancelled(), if !cancelling => {
                    cancelling = true;
                    deadline = tokio::time::Instant::now() + TIMING.termination_grace;
                    // Stop the producer first so cancellation cannot sit behind an
                    // unbounded upload. Only two bounded frames can be in flight.
                    if send_cancellation(&sender).await.is_err() {
                        return Err(unknown(stage, "Build cancellation could not be delivered; termination was not confirmed").with_work(evidence));
                    }
                },
                () = tokio::time::sleep_until(deadline) => return Err(unknown(stage, "Build response deadline expired; termination was not confirmed").with_work(evidence)),
            }
        }
    }.await;
    match outcome {
        Ok(images) => Completion::Images {
            images,
            stream: Arc::new(Mutex::new(
                retained.expect("validated loaded images retain their response stream"),
            )),
        },
        Err(failure) => Completion::Failed(failure),
    }
}

fn produce(
    root: &std::path::Path,
    sender: &mpsc::Sender<ployz_core::OpaquePayload>,
    stop: &CancellationToken,
    runtime: &tokio::runtime::Handle,
) -> Result<(), remote::InputError> {
    remote::upload(root, |frame| {
        let payload = remote::encode(&frame)?;
        runtime.block_on(send_upload_frame(sender, stop, payload))
    })
}

async fn send_upload_frame(
    sender: &mpsc::Sender<ployz_core::OpaquePayload>,
    stop: &CancellationToken,
    payload: ployz_core::OpaquePayload,
) -> Result<(), remote::InputError> {
    tokio::select! {
        biased;
        () = stop.cancelled() => Err("Build upload cancelled".into()),
        sent = sender.send(payload) => sent.map_err(|_| "Build upload stream closed".into()),
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

/// The images a finished Build made, each with its receipt when a free Build made it: a
/// Full Build's are named by tags, and a free Build's by nothing but an ID its receipt
/// names.
fn validate_outcome(
    outcome: Outcome,
    (expected, access): (MachineId, Access),
    count: usize,
    evidence: ployz_build::WorkEvidence,
) -> Result<Vec<(ployz_build::BuiltImage, Option<ployz_core::FreeReceipt>)>, RemoteBuildFailure> {
    let images: Vec<(ployz_build::BuiltImage, Option<ployz_core::FreeReceipt>)> =
        match (outcome, access) {
            (Outcome::Images { machine_id, images }, Access::Full)
                if machine_id == expected && images.len() == count =>
            {
                images.into_iter().map(|image| (image, None)).collect()
            }
            (Outcome::FreeImages { machine_id, images }, Access::Free)
                if machine_id == expected && images.len() == count =>
            {
                images
                    .into_iter()
                    .map(|free| (free.image, Some(free.receipt)))
                    .collect()
            }
            (
                Outcome::Failed {
                    stage,
                    message,
                    work,
                },
                _,
            ) => {
                return Err(RemoteBuildFailure::Failed {
                    stage,
                    message,
                    work: observed_first(work, evidence),
                });
            }
            (
                Outcome::Unknown {
                    stage,
                    message,
                    work,
                },
                _,
            ) => {
                return Err(RemoteBuildFailure::Unknown {
                    stage,
                    message,
                    work: observed_first(work, evidence),
                });
            }
            (
                Outcome::CapabilitiesChecked { .. }
                | Outcome::Images { .. }
                | Outcome::FreeImages { .. }
                | Outcome::Validated { .. }
                | Outcome::Published { .. },
                _,
            ) => {
                return Err(unknown(
                    Stage::Output,
                    "Build result does not match the admitted request",
                )
                .with_work(evidence));
            }
        };
    let invalid = |(image, free): &(ployz_build::BuiltImage, Option<ployz_core::FreeReceipt>)| {
        ployz_core::ReportedImageDigest::parse(image.reference.as_str()).is_err()
            || image.platforms.is_empty()
            || image
                .platforms
                .iter()
                .any(|platform| !remote::linux_platform(platform))
            || match free {
                None => image.tags.is_empty(),
                Some(receipt) => {
                    !image.tags.is_empty() || receipt.image.as_str() != image.reference
                }
            }
    };
    if images.iter().any(invalid) {
        return Err(unknown(
            Stage::Output,
            "Build returned an invalid image identity or platform",
        )
        .with_work(evidence));
    }
    Ok(images)
}

/// `work` as the Build Machine reported it, plus what this client saw of the targets it left out.
fn observed_first(
    mut work: ployz_build::WorkEvidence,
    observed: ployz_build::WorkEvidence,
) -> ployz_build::WorkEvidence {
    for (name, seen) in observed.0 {
        work.0.entry(name).or_insert(seen);
    }
    work
}

fn disconnected(status: tonic::Status) -> String {
    format!(
        "Build stream disconnected: {}",
        error_chain::row(&crate::connect::TransportError::from(status))
    )
}

pub(super) fn failed(stage: Stage, message: impl Into<String>) -> RemoteBuildFailure {
    RemoteBuildFailure::Failed {
        work: Default::default(),
        stage,
        message: message.into(),
    }
}
fn unknown(stage: Stage, message: impl Into<String>) -> RemoteBuildFailure {
    RemoteBuildFailure::Unknown {
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
        use ployz_build::{BuiltImage, TargetEvidence, WorkEvidence};
        let machine_id = MachineId::random();
        let image = BuiltImage {
            reference: format!("sha256:{}", "1".repeat(64)),
            tags: vec!["example.test/api:built".into()],
            platforms: vec!["linux/arm/v7".into()],
            location: "unix:///var/run/docker.sock".into(),
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
            Outcome::CapabilitiesChecked { machine_id },
            Outcome::Images {
                machine_id,
                images: vec![BuiltImage {
                    platforms: vec!["linux//v7".into()],
                    ..image.clone()
                }],
            },
        ] {
            let Err(RemoteBuildFailure::Unknown { work: retained, .. }) =
                validate_outcome(outcome, (machine_id, Access::Full), 1, work.clone())
            else {
                panic!("malformed success was accepted")
            };
            assert_eq!(retained.0, work.0);
        }
        for reference in [
            format!("sha256:{}", "1".repeat(63)),
            format!("sha256:{}g", "1".repeat(63)),
            "1".repeat(64),
        ] {
            let Err(RemoteBuildFailure::Unknown { message, .. }) = validate_outcome(
                Outcome::Images {
                    machine_id,
                    images: vec![BuiltImage {
                        reference: reference.clone(),
                        ..image.clone()
                    }],
                },
                (machine_id, Access::Full),
                1,
                work.clone(),
            ) else {
                panic!("{reference} was accepted")
            };
            assert_eq!(
                message,
                "Build returned an invalid image identity or platform"
            );
        }
        // Either case of hex is an image identity, as the report carried it.
        for reference in [
            image.reference.clone(),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "aB1".repeat(21) + "f"),
        ] {
            let Ok(images) = validate_outcome(
                Outcome::Images {
                    machine_id,
                    images: vec![BuiltImage {
                        reference: reference.clone(),
                        ..image.clone()
                    }],
                },
                (machine_id, Access::Full),
                1,
                work.clone(),
            ) else {
                panic!("{reference} was refused")
            };
            assert_eq!(
                images.first().map(|(image, _)| image.reference.as_str()),
                Some(reference.as_str())
            );
        }
    }

    #[tokio::test]
    async fn a_build_refused_by_enforcement_is_told_apart_by_its_capability() {
        let (client, _server) = crate::connect::test_support::rpc_bidi_client(|_| async {
            Err::<tonic::Response<tokio_stream::Iter<std::vec::IntoIter<Result<_, _>>>>, _>(
                tonic::Status::with_details(
                    tonic::Code::PermissionDenied,
                    ployz_core::BUILD_REFUSED_BY_ENFORCEMENT,
                    serde_json::json!({ "capability": ployz_core::BUILD_CAPABILITY, "cause": [] })
                        .to_string()
                        .into(),
                ),
            )
        })
        .await;
        let error = client
            .check_build_capabilities(
                MachineId::random(),
                &[],
                Access::Full,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, CapabilityCheckError::RefusedByEnforcement(message)
                if message == ployz_core::BUILD_REFUSED_BY_ENFORCEMENT),
            "{error:?}"
        );
    }

    #[test]
    fn an_enforcement_refusal_is_a_typed_one_or_a_failure_that_says_the_sentence() {
        let failed = |message: &str| CapabilityCheckError::from(remote::InputError::from(message));
        assert!(
            CapabilityCheckError::RefusedByEnforcement("any text".into()).refused_by_enforcement()
        );
        assert!(failed(ployz_core::BUILD_REFUSED_BY_ENFORCEMENT).refused_by_enforcement());
        assert!(!failed("Build capability deadline expired").refused_by_enforcement());
        assert!(
            !failed(&format!("{} ", ployz_core::BUILD_REFUSED_BY_ENFORCEMENT))
                .refused_by_enforcement()
        );
    }

    #[tokio::test]
    async fn a_dropped_build_stream_names_its_transport_cause() {
        let (client, _server) = crate::connect::test_support::rpc_bidi_client(|_| async {
            let queued = remote::encode(&Event::Progress(Progress::Stage(Stage::Queued))).unwrap();
            Ok(tonic::Response::new(tokio_stream::iter([
                Ok(queued),
                Err(tonic::Status::unavailable("relay connection reset")),
            ])))
        })
        .await;
        let Err(Refused {
            failure: RemoteBuildFailure::Failed { stage, message, .. },
            ..
        }) = open_and_admit(
            &client,
            Input::Check(Vec::new()),
            Access::Full,
            MachineId::random(),
            &CancellationToken::new(),
            &mut |_| {},
            &Default::default(),
        )
        .await
        else {
            panic!("a dropped stream was not reported as a failed admission")
        };
        assert_eq!(stage, Stage::Admission);
        assert_eq!(
            message,
            "Build stream disconnected: relay connection reset; source was not uploaded"
        );
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

    #[tokio::test(flavor = "multi_thread")]
    async fn a_large_file_and_many_small_files_arrive_intact_and_in_order() {
        let capture = tempfile::tempdir().unwrap();
        let root = capture.path().to_owned();
        std::fs::create_dir_all(root.join("source/small")).unwrap();
        std::fs::create_dir(root.join("private")).unwrap();
        std::fs::write(root.join("compose.yaml"), "services: {}\n").unwrap();
        let large = (0..5 * 1024 * 1024 + 17)
            .map(|byte: u32| (byte % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(root.join("source/large"), &large).unwrap();
        for file in 0..2000 {
            std::fs::write(root.join(format!("source/small/{file}")), file.to_string()).unwrap();
        }
        let mut sent = Vec::new();
        remote::upload(&root, |frame| {
            sent.push(remote::encode(&frame)?.json);
            Ok(())
        })
        .unwrap();

        let (sender, mut receiver) = mpsc::channel(2);
        let runtime = tokio::runtime::Handle::current();
        let producer = tokio::task::spawn_blocking(move || {
            produce(&root, &sender, &CancellationToken::new(), &runtime)
        });
        let mut received = Vec::new();
        while let Some(frame) = receiver.recv().await {
            received.push(frame.json);
        }
        producer.await.unwrap().unwrap();
        assert_eq!(received.len(), sent.len());
        assert!(received == sent, "frames were lost, reordered or altered");
    }

    #[tokio::test]
    async fn a_stopped_upload_does_not_wait_on_a_full_channel() {
        let (sender, _receiver) = mpsc::channel(1);
        let frame = || remote::encode(&Input::Data(vec![1])).unwrap();
        let stop = CancellationToken::new();
        send_upload_frame(&sender, &stop, frame()).await.unwrap();
        let blocked = send_upload_frame(&sender, &stop, frame());
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("a stopped upload stayed blocked on a full channel")
            .unwrap_err();
    }
}
