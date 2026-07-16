use super::runner::{BuildExecutionError, infrastructure};
use crate::roles::machine::protocol::MachineBuildLogFrame;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{BuildLogChunk, MAX_BUILD_LOG_CHUNK_BYTES};
use ployz_nats::service_runtime::NatsClient;
use ployz_nats::subjects::machine_build_log;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

const BUILD_LOG_LIMIT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Default)]
pub(crate) struct BuildLogProgress {
    sequence: Arc<AtomicU64>,
    omitted_bytes: Arc<AtomicU64>,
}

impl BuildLogProgress {
    #[must_use]
    pub(crate) fn summary(&self) -> (u64, u64) {
        (
            self.sequence.load(Ordering::Acquire),
            self.omitted_bytes.load(Ordering::Acquire),
        )
    }

    fn published(&self, sequence: u64) {
        self.sequence.store(sequence, Ordering::Release);
    }

    fn omitted(&self, omitted_bytes: u64) {
        self.omitted_bytes.store(omitted_bytes, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn set_for_test(&self, sequence: u64, omitted_bytes: u64) {
        self.published(sequence);
        self.omitted(omitted_bytes);
    }
}

pub(super) async fn read_output<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    sender: mpsc::Sender<Vec<u8>>,
) {
    let mut buffer = vec![0_u8; MAX_BUILD_LOG_CHUNK_BYTES];
    loop {
        let Ok(read) = reader.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        let Some(bytes) = buffer.get(..read) else {
            return;
        };
        if sender.send(bytes.to_vec()).await.is_err() {
            return;
        }
    }
}

pub(super) struct BuildLogPublisher {
    client: NatsClient,
    machine_id: MachineId,
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
        client: NatsClient,
        machine_id: MachineId,
        operation_id: OperationId,
        platform: OciPlatform,
        secret: &str,
        progress: BuildLogProgress,
    ) -> Self {
        Self {
            client,
            machine_id,
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

    pub(super) async fn push(&mut self, bytes: &[u8]) -> Result<(), BuildExecutionError> {
        self.pending.push_str(&String::from_utf8_lossy(bytes));
        let safe = take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::More);
        self.publish_text(safe).await
    }

    pub(super) async fn finish(mut self) -> Result<PublishedLogs, BuildExecutionError> {
        let remaining =
            take_redacted_output(&mut self.pending, &self.secret, RedactionBoundary::End);
        self.publish_text(remaining).await?;
        self.client
            .flush()
            .await
            .map_err(|error| infrastructure("flush build logs", error.to_string()))?;
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
            let frame = MachineBuildLogFrame {
                operation_id: self.operation_id.clone(),
                machine_id: self.machine_id.clone(),
                platform: self.platform.clone(),
                sequence: self.sequence,
                chunk: BuildLogChunk::try_new(chunk)
                    .map_err(|error| infrastructure("frame build log", error.to_string()))?,
            };
            let payload = serde_json::to_vec(&frame)
                .map_err(|error| infrastructure("encode build log", error.to_string()))?;
            self.client
                .publish(
                    machine_build_log(&self.machine_id, &self.operation_id),
                    payload.into(),
                )
                .await
                .map_err(|error| infrastructure("publish build log", error.to_string()))?;
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
}
