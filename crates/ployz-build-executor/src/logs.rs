use super::runner::{BuildExecutionError, infrastructure};
use ployz_core::build::{BuildExecutorAssignment, BuildExecutorLogFrame};
use ployz_core::ids::OperationId;
use ployz_core::image::OciPlatform;
use ployz_core::operation::{BuildLogChunk, MAX_BUILD_LOG_CHUNK_BYTES};
use std::future::pending;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::sync::watch;

const BUILD_LOG_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
const BUILD_LOG_DELIVERY_DEADLINE: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct BuildLogProgress {
    state: watch::Sender<(u64, u64)>,
}

impl Default for BuildLogProgress {
    fn default() -> Self {
        let (state, _) = watch::channel((0, 0));
        Self { state }
    }
}

impl BuildLogProgress {
    #[must_use]
    pub fn summary(&self) -> (u64, u64) {
        *self.state.borrow()
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<(u64, u64)> {
        self.state.subscribe()
    }

    fn published(&self, sequence: u64) {
        self.state.send_if_modified(|(observed, _)| {
            if sequence <= *observed {
                return false;
            }
            *observed = sequence;
            true
        });
    }

    fn omitted(&self, omitted_bytes: u64) {
        self.state.send_if_modified(|(_, observed)| {
            if omitted_bytes <= *observed {
                return false;
            }
            *observed = omitted_bytes;
            true
        });
    }

    #[doc(hidden)]
    pub fn set_for_test(&self, sequence: u64, omitted_bytes: u64) {
        self.published(sequence);
        self.omitted(omitted_bytes);
    }
}

pub(super) async fn read_output<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    sender: mpsc::Sender<String>,
) {
    let mut buffer = vec![0_u8; MAX_BUILD_LOG_CHUNK_BYTES];
    let mut pending = Vec::new();
    while let Ok(read) = reader.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let Some(bytes) = buffer.get(..read) else {
            return;
        };
        pending.extend_from_slice(bytes);
        let output = take_utf8_output(&mut pending, Utf8Boundary::More);
        if !output.is_empty() && sender.send(output).await.is_err() {
            return;
        }
    }
    let output = take_utf8_output(&mut pending, Utf8Boundary::End);
    if !output.is_empty() {
        let _ = sender.send(output).await;
    }
}

#[derive(Clone, Copy)]
enum Utf8Boundary {
    More,
    End,
}

fn take_utf8_output(pending: &mut Vec<u8>, boundary: Utf8Boundary) -> String {
    let mut output = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                output.push_str(valid);
                pending.clear();
                return output;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                let Some(valid) = pending.get(..valid_up_to) else {
                    unreachable!("valid UTF-8 prefix is within the inspected buffer");
                };
                output.push_str(&String::from_utf8_lossy(valid));
                pending.drain(..valid_up_to);
                if let Some(error_len) = error.error_len() {
                    output.push('�');
                    pending.drain(..error_len);
                    continue;
                }
                if matches!(boundary, Utf8Boundary::End) {
                    output.push_str(&String::from_utf8_lossy(pending));
                    pending.clear();
                }
                return output;
            }
        }
    }
}

#[derive(Clone)]
pub struct BuildLogDestination {
    frames: mpsc::Sender<BuildExecutorLogFrame>,
    assignment: BuildExecutorAssignment,
}

impl BuildLogDestination {
    #[must_use]
    pub fn new(
        frames: mpsc::Sender<BuildExecutorLogFrame>,
        assignment: BuildExecutorAssignment,
    ) -> Self {
        Self { frames, assignment }
    }
}

pub(super) struct BuildLogPublisher {
    destination: BuildLogDestination,
    operation_id: OperationId,
    platform: OciPlatform,
    secret: String,
    pending: String,
    sequence: u64,
    published_bytes: u64,
    omitted_bytes: u64,
    progress: BuildLogProgress,
    delivery: DeliveryState,
}

impl BuildLogPublisher {
    pub(super) fn new(
        destination: BuildLogDestination,
        operation_id: OperationId,
        platform: OciPlatform,
        secret: &str,
        progress: BuildLogProgress,
    ) -> Self {
        Self {
            destination,
            operation_id,
            platform,
            secret: secret.to_owned(),
            pending: String::new(),
            sequence: 0,
            published_bytes: 0,
            omitted_bytes: 0,
            progress,
            delivery: DeliveryState::Open,
        }
    }

    pub(super) async fn push(
        &mut self,
        text: &str,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<PublishOutcome, BuildExecutionError> {
        self.pending.push_str(text);
        let safe = take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::More);
        self.publish_text(safe, cancelled).await
    }

    pub(super) async fn finish(
        mut self,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<PublishedLogs, BuildExecutionError> {
        let remaining =
            take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::End);
        let _ = self.publish_text(remaining, cancelled).await?;
        Ok(PublishedLogs {
            final_sequence: self.sequence,
            omitted_bytes: self.omitted_bytes,
        })
    }

    async fn publish_text(
        &mut self,
        mut text: String,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<PublishOutcome, BuildExecutionError> {
        if *cancelled.borrow() {
            self.delivery = DeliveryState::Omitting;
            self.omit(text.len());
            return Ok(PublishOutcome::Cancelled);
        }
        if matches!(self.delivery, DeliveryState::Omitting) {
            self.omit(text.len());
            return Ok(PublishOutcome::Continue);
        }
        while !text.is_empty() {
            let split =
                char_boundary_at_or_before(&text, text.len().min(MAX_BUILD_LOG_CHUNK_BYTES));
            if split == 0 {
                break;
            }
            let chunk = text[..split].to_owned();
            text.drain(..split);
            let bytes = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            if self.published_bytes.saturating_add(bytes) > BUILD_LOG_LIMIT_BYTES {
                self.delivery = DeliveryState::Omitting;
                self.omit_bytes_and_remaining(bytes, &text);
                break;
            }
            let sequence = self.sequence.saturating_add(1);
            let frame = BuildExecutorLogFrame {
                operation_id: self.operation_id.clone(),
                assignment: self.destination.assignment.clone(),
                platform: self.platform.clone(),
                sequence,
                chunk: BuildLogChunk::try_new(chunk)
                    .map_err(|error| infrastructure("frame build log", error.to_string()))?,
            };
            match deliver_frame(&self.destination.frames, frame, cancelled).await? {
                DeliveryOutcome::Delivered => {
                    self.sequence = sequence;
                    self.published_bytes = self.published_bytes.saturating_add(bytes);
                    self.progress.published(self.sequence);
                }
                DeliveryOutcome::Deadline => {
                    self.delivery = DeliveryState::Omitting;
                    self.omit_bytes_and_remaining(bytes, &text);
                    break;
                }
                DeliveryOutcome::Cancelled => {
                    self.delivery = DeliveryState::Omitting;
                    self.omit_bytes_and_remaining(bytes, &text);
                    return Ok(PublishOutcome::Cancelled);
                }
            }
        }
        Ok(PublishOutcome::Continue)
    }

    fn omit_bytes_and_remaining(&mut self, bytes: u64, remaining: &str) {
        self.omitted_bytes = self
            .omitted_bytes
            .saturating_add(bytes)
            .saturating_add(u64::try_from(remaining.len()).unwrap_or(u64::MAX));
        self.progress.omitted(self.omitted_bytes);
    }

    fn omit(&mut self, bytes: usize) {
        self.omitted_bytes = self
            .omitted_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        self.progress.omitted(self.omitted_bytes);
    }
}

#[derive(Clone, Copy)]
enum DeliveryState {
    Open,
    Omitting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublishOutcome {
    Continue,
    Cancelled,
}

#[derive(Clone, Copy)]
enum DeliveryOutcome {
    Delivered,
    Deadline,
    Cancelled,
}

async fn deliver_frame(
    frames: &mpsc::Sender<BuildExecutorLogFrame>,
    frame: BuildExecutorLogFrame,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<DeliveryOutcome, BuildExecutionError> {
    tokio::select! {
        biased;
        () = cancellation_requested(cancelled) => Ok(DeliveryOutcome::Cancelled),
        result = tokio::time::timeout(BUILD_LOG_DELIVERY_DEADLINE, frames.send(frame)) => {
            match result {
                Ok(Ok(())) => Ok(DeliveryOutcome::Delivered),
                Ok(Err(error)) => {
                    Err(infrastructure("deliver build log", error.to_string()))
                }
                Err(_) => Ok(DeliveryOutcome::Deadline),
            }
        }
    }
}

async fn cancellation_requested(cancelled: &mut watch::Receiver<bool>) {
    loop {
        if *cancelled.borrow_and_update() {
            return;
        }
        if cancelled.changed().await.is_err() {
            pending::<()>().await;
        }
    }
}

#[derive(Clone, Copy)]
enum RedactionBoundary {
    More,
    End,
}

fn take_redacted_output(pending: &mut String, secret: &str, boundary: RedactionBoundary) -> String {
    if secret.is_empty() {
        return std::mem::take(pending);
    }
    let mut safe = String::new();
    while let Some(position) = pending.find(secret) {
        safe.push_str(&pending[..position]);
        safe.push_str("[redacted]");
        pending.drain(..position + secret.len());
    }
    let keep = match boundary {
        RedactionBoundary::More => secret.len().saturating_sub(1),
        RedactionBoundary::End => 0,
    };
    let split = char_boundary_at_or_before(pending, pending.len().saturating_sub(keep));
    safe.push_str(&pending[..split]);
    pending.drain(..split);
    safe
}

pub(super) struct PublishedLogs {
    pub(super) final_sequence: u64,
    pub(super) omitted_bytes: u64,
}

fn char_boundary_at_or_before(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::MachineId;
    use tokio::io::{AsyncWriteExt, duplex};

    fn destination(
        capacity: usize,
    ) -> (BuildLogDestination, mpsc::Receiver<BuildExecutorLogFrame>) {
        let (frames, receiver) = mpsc::channel(capacity);
        let assignment = BuildExecutorAssignment::Cluster {
            machine_id: MachineId::try_new("machine-a").expect("machine id"),
        };
        (BuildLogDestination::new(frames, assignment), receiver)
    }

    fn publisher(
        destination: BuildLogDestination,
        secret: &str,
        progress: BuildLogProgress,
    ) -> BuildLogPublisher {
        BuildLogPublisher::new(
            destination,
            OperationId::try_new("operation-a").expect("operation id"),
            OciPlatform::try_new("linux", "amd64").expect("platform"),
            secret,
            progress,
        )
    }

    fn cancellation() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }

    #[tokio::test]
    async fn read_output_preserves_utf8_split_across_reads() {
        let (mut writer, reader) = duplex(1);
        let (sender, mut receiver) = mpsc::channel(1);
        let read = tokio::spawn(read_output(reader, sender));

        writer.write_all("€".as_bytes()).await.unwrap();
        writer.shutdown().await.unwrap();

        assert_eq!(receiver.recv().await.as_deref(), Some("€"));
        assert_eq!(receiver.recv().await, None);
        read.await.unwrap();
    }

    #[tokio::test]
    async fn read_output_lossily_emits_terminal_incomplete_utf8() {
        let (mut writer, reader) = duplex(1);
        let (sender, mut receiver) = mpsc::channel(1);
        let read = tokio::spawn(read_output(reader, sender));

        writer.write_all(&[0xe2, 0x82]).await.unwrap();
        writer.shutdown().await.unwrap();

        assert_eq!(receiver.recv().await.as_deref(), Some("�"));
        assert_eq!(receiver.recv().await, None);
        read.await.unwrap();
    }

    #[test]
    fn streaming_redaction_keeps_a_possible_secret_suffix() {
        let secret = "token-123";
        let mut pending = "prefix token-".to_owned();
        let first = take_redacted_output(&mut pending, secret, RedactionBoundary::More);
        pending.push_str("123 suffix");
        let second = take_redacted_output(&mut pending, secret, RedactionBoundary::End);

        assert_eq!(first + &second, "prefix [redacted] suffix");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn publisher_delivers_redacted_sequenced_frames() {
        let (destination, mut receiver) = destination(4);
        let progress = BuildLogProgress::default();
        let mut publisher = publisher(destination, "token-123", progress.clone());
        let (_cancel, mut cancelled) = cancellation();

        assert_eq!(
            publisher
                .push("first token-", &mut cancelled)
                .await
                .expect("first output"),
            PublishOutcome::Continue
        );
        assert_eq!(
            publisher
                .push("123 second", &mut cancelled)
                .await
                .expect("second output"),
            PublishOutcome::Continue
        );
        let published = publisher.finish(&mut cancelled).await.expect("finish logs");

        let mut frames = Vec::new();
        while let Some(frame) = receiver.recv().await {
            frames.push(frame);
        }
        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.chunk.as_str())
                .collect::<String>(),
            "first [redacted] second"
        );
        let last = frames.last().expect("last frame");
        assert_eq!(last.operation_id.as_str(), "operation-a");
        assert_eq!(last.platform.os(), "linux");
        assert_eq!(last.platform.architecture(), "amd64");
        assert!(matches!(
            &last.assignment,
            BuildExecutorAssignment::Cluster { machine_id }
                if machine_id.as_str() == "machine-a"
        ));
        assert_eq!(published.final_sequence, 3);
        assert_eq!(published.omitted_bytes, 0);
        assert_eq!(progress.summary(), (3, 0));
    }

    #[tokio::test]
    async fn publisher_records_output_omitted_after_limit() {
        let (destination, mut frames) = destination(1);
        let progress = BuildLogProgress::default();
        let mut publisher = publisher(destination, "", progress.clone());
        publisher.published_bytes = BUILD_LOG_LIMIT_BYTES;
        let (_cancel, mut cancelled) = cancellation();

        publisher
            .push("omitted", &mut cancelled)
            .await
            .expect("omit output");
        let published = publisher.finish(&mut cancelled).await.expect("finish logs");

        assert!(frames.try_recv().is_err());
        assert_eq!(published.final_sequence, 0);
        assert_eq!(published.omitted_bytes, 7);
        assert_eq!(progress.summary(), (0, 7));
    }

    #[tokio::test]
    async fn publisher_reports_a_closed_destination() {
        let (destination, frames) = destination(1);
        drop(frames);
        let mut publisher = publisher(destination, "", BuildLogProgress::default());
        let (_cancel, mut cancelled) = cancellation();

        let error = publisher
            .push("output", &mut cancelled)
            .await
            .expect_err("closed output");

        assert!(matches!(
            error,
            BuildExecutionError::Infrastructure { action, .. }
                if action == "deliver build log"
        ));
    }

    #[tokio::test]
    async fn cancellation_preempts_a_saturated_destination() {
        let (destination, mut frames) = destination(1);
        let progress = BuildLogProgress::default();
        let mut publisher = publisher(destination, "", progress.clone());
        let (cancel, mut cancelled) = cancellation();
        publisher
            .push("published", &mut cancelled)
            .await
            .expect("first frame fits");

        let cancellation = async {
            tokio::task::yield_now().await;
            cancel
                .send(true)
                .expect("cancellation receiver remains live");
        };
        let delivery = publisher.push("omitted", &mut cancelled);
        let (outcome, ()) = tokio::join!(delivery, cancellation);

        assert_eq!(
            outcome.expect("cancelled delivery is accounted"),
            PublishOutcome::Cancelled
        );
        let published = publisher
            .finish(&mut cancelled)
            .await
            .expect("cancelled finish does not wait for receiver capacity");
        let frame = frames.recv().await.expect("first frame remains evidence");
        assert_eq!(frame.chunk.as_str(), "published");
        assert!(frames.try_recv().is_err());
        assert_eq!(published.final_sequence, 1);
        assert_eq!(published.omitted_bytes, 7);
        assert_eq!(progress.summary(), (1, 7));
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_deadline_degrades_to_omitted_evidence_once() {
        let (destination, mut frames) = destination(1);
        let progress = BuildLogProgress::default();
        let mut publisher = publisher(destination, "", progress.clone());
        let (_cancel, mut cancelled) = cancellation();
        publisher
            .push("published", &mut cancelled)
            .await
            .expect("first frame fits");

        assert_eq!(
            publisher
                .push("deadline", &mut cancelled)
                .await
                .expect("deadline omits rather than fails the build"),
            PublishOutcome::Continue
        );
        assert_eq!(
            publisher
                .push(" overflow", &mut cancelled)
                .await
                .expect("later output is omitted without another wait"),
            PublishOutcome::Continue
        );
        let published = publisher
            .finish(&mut cancelled)
            .await
            .expect("finish is immediate after saturation");

        let frame = frames.recv().await.expect("first frame remains evidence");
        assert_eq!(frame.chunk.as_str(), "published");
        assert!(frames.try_recv().is_err());
        assert_eq!(published.final_sequence, 1);
        assert_eq!(published.omitted_bytes, 17);
        assert_eq!(progress.summary(), (1, 17));
    }

    #[tokio::test(start_paused = true)]
    async fn finish_has_the_same_delivery_deadline() {
        let (destination, mut frames) = destination(1);
        let progress = BuildLogProgress::default();
        let mut publisher = publisher(destination, "secret", progress.clone());
        let (_cancel, mut cancelled) = cancellation();
        publisher
            .push("publishedsecre", &mut cancelled)
            .await
            .expect("safe prefix fills the destination");

        let published = publisher
            .finish(&mut cancelled)
            .await
            .expect("terminal redaction output is deadline bounded");

        let frame = frames.recv().await.expect("safe prefix remains evidence");
        assert_eq!(frame.chunk.as_str(), "published");
        assert!(frames.try_recv().is_err());
        assert_eq!(published.final_sequence, 1);
        assert_eq!(published.omitted_bytes, 5);
        assert_eq!(progress.summary(), (1, 5));
    }

    #[tokio::test]
    async fn published_and_omitted_output_each_signal_activity() {
        let progress = BuildLogProgress::default();
        let mut activity = progress.subscribe();

        progress.published(1);
        activity.changed().await.expect("published activity");
        let published_generation = *activity.borrow_and_update();
        progress.omitted(64);
        activity.changed().await.expect("omitted activity");

        assert!(*activity.borrow() > published_generation);
        assert_eq!(progress.summary(), (1, 64));
    }

    #[test]
    fn activity_only_advances_for_component_wise_strict_progress() {
        let progress = BuildLogProgress::default();
        let activity = progress.subscribe();

        progress.set_for_test(3, 5);
        let initial = *activity.borrow();
        progress.set_for_test(3, 5);
        progress.set_for_test(2, 4);
        assert_eq!(*activity.borrow(), initial);
        assert_eq!(progress.summary(), (3, 5));

        progress.set_for_test(4, 5);
        assert_eq!(*activity.borrow(), (4, 5));
        progress.set_for_test(4, 6);
        assert_eq!(*activity.borrow(), (4, 6));
        assert_eq!(progress.summary(), (4, 6));
    }
}
