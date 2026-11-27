//! Admission and lifetime of one connection-scoped remote Build.

mod queue;
pub(crate) use queue::Runner;

use crate::logs::RpcStream;
use crate::sealed_env::{SealedRecord, SealingKeyStore};
use ployz_build::{
    Access, Admission, BuildError, HostPolicy, Output, Progress, Stage,
    remote::{self, Definition, Event, Input, Outcome},
};
use ployz_core::{BuildConcurrency, MachineId, OpaquePayload, OpenedSealed};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::mpsc;
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tonic::Status;

const BUILD_ENDED: &str = "Build stopped: this Device no longer has Full access to this Server.";

/// A Build that did not confirm its builder stopped, in its own words.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Unconfirmed(String);

#[derive(Default)]
struct AttemptState {
    evidence: Mutex<ployz_build::WorkEvidence>,
    retained: Mutex<Option<RetainedBuild>>,
    /// Set once the Build runs Docker, from which point it may have loaded an image.
    ran: std::sync::atomic::AtomicBool,
}

struct RetainedBuild {
    // Field order keeps installation exclusion until Docker tag cleanup finishes.
    _retention: ployz_build::ImageRetention,
    _installation: crate::mutation::MutationGuard,
}

struct ExecutionAdmission {
    build: Admission,
    installation: crate::mutation::MutationGuard,
}

/// Run one Build with `access`: a Full caller's, or a free one that any caller may start.
pub(crate) fn start(
    local: crate::machine::LocalMachine,
    requests: impl Stream<Item = Result<OpaquePayload, Status>> + Send + Unpin + 'static,
    runner: std::sync::Arc<Runner>,
    sealing: SealingKeyStore,
    access: Access,
) -> RpcStream {
    let (events, receiver) = mpsc::channel(8);
    tokio::spawn(async move {
        let shutdown = runner.shutdown.clone();
        let state = Arc::new(AttemptState::default());
        let cleanup = local.clone();
        let free = access == Access::Free;
        let outcome = attempt(
            local,
            requests,
            &events,
            runner,
            state.clone(),
            (sealing, access),
        )
        .await;
        let message = "Build terminal report exceeds the response size limit";
        let fallback = match &outcome {
            Outcome::Unknown { stage, .. } => Outcome::Unknown {
                stage: *stage,
                message: message.into(),
                work: Default::default(),
            },
            Outcome::Failed { stage, .. } => failed(*stage, message),
            Outcome::CapabilitiesChecked { .. }
            | Outcome::Images { .. }
            | Outcome::FreeImages { .. }
            | Outcome::Validated { .. }
            | Outcome::Published { .. } => {
                failed(Stage::Output, format!("Build completed; {message}"))
            }
        };
        let payload = remote::encode(&Event::Finished(outcome)).unwrap_or_else(|_| {
            remote::encode(&Event::Finished(fallback)).expect("bounded terminal fallback")
        });
        let _ = tokio::time::timeout(Duration::from_secs(5), events.send(Ok(payload))).await;
        // A Free Build that ran may have loaded images whatever its outcome, one target at
        // a time, and nothing else removes them on a Server only Cloud deploys to.
        if free
            && state.ran.load(std::sync::atomic::Ordering::Acquire)
            && let Err(error) = cleanup.prune_free_images().await
        {
            tracing::warn!(
                error = %ployz_core::error_chain::inline(&error),
                "Free Build image cleanup failed"
            );
        }
        // Installation exclusion follows retained image cleanup because
        // releasing temporary tags mutates Docker.
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

#[derive(Debug, thiserror::Error)]
enum BuildAdmissionError {
    #[error(transparent)]
    Local(#[from] crate::machine::LocalMachineError),
    #[error("Machine is not participating; Build execution was not attempted")]
    NotParticipating,
    #[error("Machine does not accept Builds; execution was not attempted")]
    Disabled,
}

fn require_build_acceptance(
    local: &crate::machine::LocalMachine,
) -> Result<(MachineId, BuildConcurrency), BuildAdmissionError> {
    let record = local.record();
    let machine = record
        .machine()
        .ok_or(BuildAdmissionError::NotParticipating)?;
    if !machine.accepts_builds {
        return Err(BuildAdmissionError::Disabled);
    }
    Ok((machine.id, machine.effective_build_concurrency()))
}

async fn attempt(
    local: crate::machine::LocalMachine,
    mut requests: impl Stream<Item = Result<OpaquePayload, Status>> + Send + Unpin + 'static,
    events: &mpsc::Sender<Result<OpaquePayload, Status>>,
    runner: Arc<Runner>,
    state: Arc<AttemptState>,
    (sealing, access): (SealingKeyStore, Access),
) -> Outcome {
    let start = tokio::time::timeout(Duration::from_secs(10), requests.next()).await;
    let request = match start {
        Ok(Some(Ok(payload))) => match remote::decode(&payload) {
            Ok(request @ (Input::Start(_) | Input::Check(_))) => request,
            _ => return failed(Stage::Admission, "expected a valid Build start frame"),
        },
        _ => return failed(Stage::Admission, "Build request ended before admission"),
    };
    let targets = match &request {
        Input::Start(definition) if definition.image_contexts.len() <= 128 => &definition.targets,
        Input::Check(targets)
            if targets
                .iter()
                .flat_map(|target| &target.platforms)
                .all(|platform| remote::linux_platform(platform)) =>
        {
            targets
        }
        Input::Check(_)
        | Input::Start(_)
        | Input::Entry { .. }
        | Input::Data(_)
        | Input::Finish
        | Input::Cancel => {
            return failed(
                Stage::Admission,
                "invalid Build capability request or too many image contexts",
            );
        }
    };
    if targets.is_empty() || targets.len() > 128 {
        return failed(
            Stage::Admission,
            "Build must name between one and 128 targets",
        );
    }
    let work = ployz_build::WorkEvidence::new(targets);
    if let Err(reason) = require_build_acceptance(&local) {
        return failed(Stage::Admission, reason.to_string()).with_work(work);
    }
    let bound = match access {
        // Only a Full caller builds with Sealed build values, logins and other Services'
        // images. Bound before it queues, so a key change that leaves the key short of
        // Full ends it queued or running, and returns only once it stopped.
        Access::Full => match local.bind_full(crate::machine::Work::Build) {
            Ok(bound) => bound,
            Err(error) => {
                return failed(Stage::Admission, ployz_core::error_chain::inline(&error))
                    .with_work(work);
            }
        },
        // A Free Build reads none of those, so no key's scope bounds it. It is refused
        // before it queues when it asks for another Service's image or a tag.
        Access::Free => {
            if let Input::Start(definition) = &request
                && let Some(refusal) = definition.free_refusal()
            {
                return failed(
                    Stage::Admission,
                    format!("{refusal}; execution was not attempted"),
                )
                .with_work(work);
            }
            crate::machine::Bound::unbound()
        }
    };
    let admitted = async {
        // The client takes one Queued, whichever wait comes first.
        let mut queued = false;
        let mut permit = match runner.enter() {
            Ok(queue::Entry::Active(permit)) => permit,
            Ok(queue::Entry::Waiting(waiting)) => {
                if !std::mem::replace(&mut queued, true) && events.send(Ok(queued_frame())).await.is_err() {
                    return Err(failed(Stage::Queued, "Build client disconnected").with_work(work.clone()));
                }
                tokio::select! {
                    biased;
                    () = runner.shutdown.cancelled() => return Err(failed(Stage::Queued, "Build daemon stopped; execution was not attempted").with_work(work.clone())),
                    () = events.closed() => return Err(failed(Stage::Queued, "Build client disconnected").with_work(work.clone())),
                    () = bound.ended() => return Err(failed(Stage::Queued, format!("{BUILD_ENDED} Execution was not attempted.")).with_work(work.clone())),
                    frame = requests.next() => return Err(failed(Stage::Queued, left_queue(frame)).with_work(work.clone())),
                    admitted = runner.admit(waiting) => match admitted {
                        Ok(permit) => permit,
                        Err(reason) => return Err(failed(Stage::Queued, reason.to_string()).with_work(work.clone())),
                    },
                }
            }
            Err(reason) => return Err(failed(Stage::Queued, reason.to_string()).with_work(work.clone())),
        };
        // Startup may still be cleaning abandoned slots. That is a queue wait:
        // the client hears Queued, so it waits past its initial deadline.
        let mut cleaned = runner.cleaned();
        if !*cleaned.borrow_and_update() {
            if !std::mem::replace(&mut queued, true) && events.send(Ok(queued_frame())).await.is_err() {
                return Err(failed(Stage::Queued, "Build client disconnected").with_work(work.clone()));
            }
            tokio::select! {
                biased;
                () = runner.shutdown.cancelled() => return Err(failed(Stage::Queued, "Build daemon stopped; execution was not attempted").with_work(work.clone())),
                () = events.closed() => return Err(failed(Stage::Queued, "Build client disconnected").with_work(work.clone())),
                () = bound.ended() => return Err(failed(Stage::Queued, format!("{BUILD_ENDED} Execution was not attempted.")).with_work(work.clone())),
                frame = requests.next() => return Err(failed(Stage::Queued, left_queue(frame)).with_work(work.clone())),
                _ = cleaned.wait_for(|cleaned| *cleaned) => {}
            }
        }
        // A Build takes only its build slot and installation exclusion. It never
        // holds the Machine's mutation lock, so deploys on this Machine proceed.
        let installation = local.admit_installation().map_err(|error| {
            failed(Stage::Admission, ployz_core::error_chain::inline(&error)).with_work(work.clone())
        })?;
        // Acceptance may have been revoked while this Build waited in the queue.
        let (machine_id, concurrency) = require_build_acceptance(&local)
            .map_err(|reason| failed(Stage::Admission, reason.to_string()).with_work(work.clone()))?;
        // Admission may wait for another one to leave the gate, or for Docker
        // to clean a slot it recovers. That is a queue wait: the client hears
        // Queued, and a cancel or disconnect ends it, freeing the installation
        // exclusion. A slot the abandoned admission takes later goes with it.
        let (waiting, mut waited) = tokio::sync::oneshot::channel();
        let mut acquiring = tokio::task::spawn_blocking({
            let policy = runner.policy.clone();
            move || {
                Admission::try_acquire_waiting(&policy, concurrency, move || {
                    let _ = waiting.send(());
                })
                .map(|admission| admission.with_access(access))
            }
        });
        let mut heard = false;
        let admission = loop {
            tokio::select! {
                biased;
                () = runner.shutdown.cancelled() => return Err(failed(Stage::Queued, "Build daemon stopped; execution was not attempted").with_work(work.clone())),
                () = events.closed() => return Err(failed(Stage::Queued, "Build client disconnected").with_work(work.clone())),
                () = bound.ended() => return Err(failed(Stage::Queued, format!("{BUILD_ENDED} Execution was not attempted.")).with_work(work.clone())),
                frame = requests.next(), if queued => return Err(failed(Stage::Queued, left_queue(frame)).with_work(work.clone())),
                notice = &mut waited, if !heard => {
                    heard = true;
                    if notice.is_ok() && !std::mem::replace(&mut queued, true) && events.send(Ok(queued_frame())).await.is_err() {
                        return Err(failed(Stage::Queued, "Build client disconnected").with_work(work.clone()));
                    }
                }
                acquired = &mut acquiring => match acquired {
                    Ok(Ok(admission)) => break admission,
                    Ok(Err(error)) => return Err(failure(Stage::Admission, error).with_work(work.clone())),
                    Err(_) => return Err(failed(Stage::Admission, "Build admission task failed")),
                },
            }
        };
        let admitted = remote::encode(&Event::Admitted {
            machine_id,
            active_timeout: runner.policy.active_timeout,
        })
        .expect("bounded admission frame");
        tokio::select! {
            sent = events.send(Ok(admitted)) => if sent.is_err() {
                return Err(failed(Stage::Admission, "Build client disconnected"));
            },
            () = bound.ended() => return Err(failed(Stage::Admission, BUILD_ENDED)),
        }
        if matches!(request, Input::Start(_)) {
            permit.count_as_build(&runner);
        }
        Ok((permit, installation, admission, machine_id))
    }
    .await;
    let (permit, installation, admission, machine_id) = match admitted {
        Ok(admitted) => admitted,
        Err(outcome) => {
            // Nothing of this Build ran before its admission.
            bound.stopped(crate::machine::Seen::NeverSent);
            return outcome;
        }
    };
    let policy = runner.policy.clone();
    *state.evidence.lock().expect("Build evidence lock") = work;
    let observed = state.clone();
    let cancellation = admission.cancellation();
    let staged = admission.sealed_staging();
    let slot = admission.slot();
    let remaining = admission.remaining();
    let deadline = tokio::time::Instant::now() + remaining;
    let (upload, source) = mpsc::channel(2);
    let output = events.clone();
    let tie = bound.tie().clone();
    #[cfg(test)]
    let returning = runner
        .returning
        .lock()
        .expect("returning hook lock")
        .clone();
    // The execution holds the Build bound until it has returned, which may be after this
    // attempt reported it Unknown: a key change that ends it waits for the execution
    // itself, and hears when its builder didn't confirm it stopped.
    let mut execution = tokio::task::spawn_blocking(move || {
        let outcome = match request {
            Input::Start(definition) => receive_and_execute(
                machine_id,
                definition,
                source,
                ExecutionAdmission {
                    build: admission,
                    installation,
                },
                policy.clone(),
                (&sealing, access),
                &bound,
                &output,
                &observed,
            ),
            Input::Check(targets) => {
                // The input pump still handles cancellation; no upload is admitted.
                drop(source);
                let _installation = installation;
                match admission.check_capabilities(&policy.docker, &targets) {
                    Ok(()) => Outcome::CapabilitiesChecked { machine_id },
                    Err(error) => failure(Stage::Preparation, error),
                }
            }
            Input::Entry { .. } | Input::Data(_) | Input::Finish | Input::Cancel => {
                unreachable!("only Build starts and capability checks reach admission")
            }
        };
        // The Build stops counting as running before a key change that ends it can hear it
        // stopped. Its builder was already released, or left quarantined, as it returned.
        drop(permit);
        // The execution returned, so every process it started was reaped, unless it says
        // Unknown: then the build slot it leaves quarantined says when they are gone.
        match &outcome {
            Outcome::Unknown { message, .. } => bound.unconfirmed(
                Unconfirmed(message.clone()),
                Some(Arc::new(move || {
                    let policy = policy.clone();
                    Box::pin(async move {
                        let confirmed =
                            tokio::task::spawn_blocking(move || policy.confirm_ended(slot))
                                .await
                                .map_err(|_| Unconfirmed("the Build slot's check failed".into()))?;
                        match confirmed {
                            Ok(()) => Ok(crate::machine::Seen::Reaped),
                            // Another admission holds the slot, perhaps recovering it in
                            // vain while the Build's processes still run.
                            Err(ployz_build::BuildError::Busy) => Ok(crate::machine::Seen::Busy),
                            Err(error) => Err(error.into()),
                        }
                    })
                })),
            ),
            Outcome::Failed { .. }
            | Outcome::CapabilitiesChecked { .. }
            | Outcome::Images { .. }
            | Outcome::FreeImages { .. }
            | Outcome::Validated { .. }
            | Outcome::Published { .. } => bound.stopped(crate::machine::Seen::Joined),
        }
        #[cfg(test)]
        if let Some(returning) = returning {
            returning();
        }
        outcome
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
            () = tie.ended() => BUILD_ENDED.to_owned(),
        }
    };
    cancellation.cancel();
    // An ended Build keeps no opened value, even while its builder takes time to stop.
    if tie.is_ended() {
        revoke(&staged);
    }
    // Termination and cleanup have a bounded grace period. A task that cannot
    // confirm its end keeps both its admission and persistent quarantine.
    let outcome = match tokio::time::timeout(Duration::from_secs(70), execution).await {
        Ok(result) => match joined(result) {
            outcome @ Outcome::Unknown { .. } => outcome,
            Outcome::CapabilitiesChecked { .. }
            | Outcome::Images { .. }
            | Outcome::FreeImages { .. }
            | Outcome::Validated { .. }
            | Outcome::Published { .. } => failed(
                Stage::Cleanup,
                format!("{reason}; output completed before cancellation was observed"),
            ),
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
    // Unknown holds no opened value: one a builder still running staged is taken back.
    if matches!(outcome, Outcome::Unknown { .. }) {
        revoke(&staged);
    }
    outcome.with_work(state.evidence.lock().expect("Build evidence lock").clone())
}

fn revoke(staged: &ployz_build::SealedStaging) {
    if let Err(error) = staged.revoke() {
        tracing::warn!(error = %ployz_core::error_chain::inline(&error), "could not remove a stopped Build's opened values");
    }
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

#[expect(
    clippy::too_many_arguments,
    reason = "one admitted Build's parts, moved into its blocking task"
)]
fn receive_and_execute(
    machine_id: MachineId,
    mut definition: Definition,
    mut source: mpsc::Receiver<OpaquePayload>,
    admission: ExecutionAdmission,
    policy: HostPolicy,
    (sealing, access): (&SealingKeyStore, Access),
    bound: &crate::machine::Bound,
    events: &mpsc::Sender<Result<OpaquePayload, Status>>,
    state: &AttemptState,
) -> Outcome {
    let ExecutionAdmission {
        build: admission,
        installation,
    } = admission;
    let cancellation = admission.cancellation();
    let deadline = std::time::Instant::now() + admission.remaining();
    let mut upload = match admission.upload() {
        Ok(upload) => upload,
        Err(error) => return failed(Stage::Upload, ployz_core::error_chain::inline(&error)),
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
            Err(error) => return failed(Stage::Upload, ployz_core::error_chain::inline(&error)),
        };
        let finished = matches!(frame, Input::Finish);
        if let Err(error) = upload.accept(frame) {
            return failed(Stage::Upload, ployz_core::error_chain::inline(&error));
        }
        if finished {
            break;
        }
    }
    // A Free Build's upload refused any Sealed record as it arrived; none is looked for.
    let record = match access {
        Access::Full => upload.private_file(ployz_build::capture::SEALED_RECORD),
        Access::Free => Ok(None),
    };
    let sealed = match record {
        Ok(None) => None,
        Ok(Some(record)) => match bound.read(|| open_sealed(&record, sealing)) {
            Ok(Ok(opened)) => {
                let scrub = ployz_build::OutputScrub::new(&opened);
                Some((opened, scrub))
            }
            Ok(Err(message)) => return failed(Stage::Preparation, message),
            Err(_) => return failed(Stage::Preparation, BUILD_ENDED),
        },
        Err(error) => return failed(Stage::Upload, ployz_core::error_chain::inline(&error)),
    };
    let send = |progress| {
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
    let progress = |progress| match &sealed {
        Some((_, scrub)) => scrub.progress(progress).into_iter().for_each(&send),
        None => send(progress),
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
            Err(error) => {
                return failed(Stage::Preparation, ployz_core::error_chain::inline(&error));
            }
        };
        context.source.management_address =
            ployz_core::ManagementAddress(std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped());
        context.source.port = proxy.port;
        proxies.push(proxy);
    }
    state.ran.store(true, std::sync::atomic::Ordering::Release);
    let result = upload.execute(
        &definition,
        Some(&policy.docker),
        sealed.as_ref().map(|(opened, _)| opened),
        &progress,
    );
    // Reached however the Build ended, so a cancelled one releases no held text.
    if let Some((_, scrub)) = &sealed {
        scrub.finish().into_iter().for_each(&send);
    }
    let outcome = match result {
        Ok(completed) => {
            if !definition.retained_tags.is_empty() {
                *state.retained.lock().expect("Build retention lock") = Some(RetainedBuild {
                    _retention: completed.retention,
                    _installation: installation,
                });
            }
            // A Free Build only loads: its definition was refused otherwise.
            match access {
                Access::Free => free_images(machine_id, completed.images, sealing),
                Access::Full => match definition.output {
                    Output::Load => Outcome::Images {
                        machine_id,
                        images: completed.images,
                    },
                    Output::Validate => Outcome::Validated { machine_id },
                    Output::Registry => Outcome::Published { machine_id },
                },
            }
        }
        Err(error) => failure(error.stage(), error),
    };
    match &sealed {
        Some((_, scrub)) => scrub.outcome(outcome),
        None => outcome,
    }
}

/// A Free Build's images, each with this Cluster's receipt for its ID.
fn free_images(
    machine_id: MachineId,
    images: Vec<ployz_build::BuiltImage>,
    sealing: &SealingKeyStore,
) -> Outcome {
    let receipts = images
        .into_iter()
        .map(|image| {
            let id = ployz_core::ImageDigest::parse(&image.reference).map_err(|error| {
                format!("the free Build's image ID can't be vouched for: {error}")
            })?;
            let receipt = sealing.free_receipt(&id).map_err(|error| {
                format!(
                    "this Server can't vouch for its free Build's image: {}",
                    ployz_core::error_chain::inline(&error)
                )
            })?;
            Ok(remote::FreeImage { image, receipt })
        })
        .collect::<Result<Vec<_>, String>>();
    match receipts {
        Ok(images) => Outcome::FreeImages { machine_id, images },
        Err(message) => failed(Stage::Output, message),
    }
}

fn joined(result: Result<Outcome, tokio::task::JoinError>) -> Outcome {
    result.unwrap_or_else(|_| Outcome::Unknown {
        work: Default::default(),
        stage: Stage::Cleanup,
        message: "Build task failed; termination cannot be confirmed".into(),
    })
}
/// Open the Build's Sealed record. The message names a variable at most, never what
/// it holds.
fn open_sealed(record: &[u8], sealing: &SealingKeyStore) -> Result<OpenedSealed, String> {
    let record: SealedRecord = serde_json::from_slice(record)
        .map_err(|_| "the Build's sealed values are not a valid record".to_owned())?;
    sealing
        .open_for_build(&record)
        .map_err(|error| error.to_string())
}

fn queued_frame() -> OpaquePayload {
    remote::encode(&Event::Progress(Progress::Stage(Stage::Queued))).expect("bounded queue frame")
}

/// Why a queued Build ended on its client's next frame.
fn left_queue(frame: Option<Result<OpaquePayload, Status>>) -> &'static str {
    match frame {
        Some(Ok(payload)) if matches!(remote::decode::<Input>(&payload), Ok(Input::Cancel)) => {
            "Build cancelled while queued; execution was not attempted"
        }
        None | Some(Err(_)) => {
            "Build client disconnected while queued; execution was not attempted"
        }
        Some(Ok(_)) => "Build upload before admission is forbidden; execution was not attempted",
    }
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
            message: ployz_core::error_chain::inline(&error),
        }
    } else {
        failed(stage, ployz_core::error_chain::inline(&error))
    }
}

#[cfg(test)]
mod tests;
