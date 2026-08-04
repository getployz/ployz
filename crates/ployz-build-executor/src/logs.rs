use super::runner::{BuildExecutionError, infrastructure};
use ployz_core::build::{BuildExecutorAssignment, BuildExecutorLogFrame};
use ployz_core::ids::OperationId;
use ployz_core::image::OciPlatform;
use ployz_core::operation::{BuildLogChunk, MAX_BUILD_LOG_CHUNK_BYTES};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::sync::watch;

const BUILD_LOG_LIMIT_BYTES: u64 = 8 * 1024 * 1024;

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
        }
    }

    pub(super) async fn push(&mut self, text: &str) -> Result<(), BuildExecutionError> {
        self.pending.push_str(text);
        let safe = take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::More);
        self.publish_text(safe).await
    }

    pub(super) async fn finish(mut self) -> Result<PublishedLogs, BuildExecutionError> {
        let remaining =
            take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::End);
        self.publish_text(remaining).await?;
        Ok(PublishedLogs {
            final_sequence: self.sequence,
            omitted_bytes: self.omitted_bytes,
        })
    }

    async fn publish_text(&mut self, mut text: String) -> Result<(), BuildExecutionError> {
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
                self.omitted_bytes = self
                    .omitted_bytes
                    .saturating_add(bytes)
                    .saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
                self.progress.omitted(self.omitted_bytes);
                break;
            }
            self.sequence = self.sequence.saturating_add(1);
            let frame = BuildExecutorLogFrame {
                operation_id: self.operation_id.clone(),
                assignment: self.destination.assignment.clone(),
                platform: self.platform.clone(),
                sequence: self.sequence,
                chunk: BuildLogChunk::try_new(chunk)
                    .map_err(|error| infrastructure("frame build log", error.to_string()))?,
            };
            self.destination
                .frames
                .send(frame)
                .await
                .map_err(|error| infrastructure("deliver build log", error.to_string()))?;
            self.published_bytes = self.published_bytes.saturating_add(bytes);
            self.progress.published(self.sequence);
        }
        Ok(())
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

        publisher.push("first token-").await.expect("first output");
        publisher.push("123 second").await.expect("second output");
        let published = publisher.finish().await.expect("finish logs");

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

        publisher.push("omitted").await.expect("omit output");
        let published = publisher.finish().await.expect("finish logs");

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

        let error = publisher.push("output").await.expect_err("closed output");

        assert!(matches!(
            error,
            BuildExecutionError::Infrastructure { action, .. }
                if action == "deliver build log"
        ));
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
