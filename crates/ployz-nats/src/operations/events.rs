use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use async_nats::jetstream::message::StreamMessage;
use async_nats::jetstream::stream::{LastRawMessageErrorKind, Stream};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    EventSequence, EventSequenceError, OperationEvent, OperationEventReplayLimit,
    OperationEventReplayPage, ReplayedOperationEvent,
};
use ployz_core::subjects::op_watch;

use crate::kv::{NatsIoTimeout, with_io_timeout};
use crate::streams::MessageId;

use super::PLZ_OPS_STREAM;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEventAppend {
    subject: String,
    message_id: MessageId,
    payload: OperationEvent,
}

impl OperationEventAppend {
    /// Wraps an event for append; the subject and idempotent message id come
    /// from the event itself ([`OperationEvent::subject`],
    /// [`OperationEvent::message_id`]).
    #[must_use]
    pub fn from_event(payload: OperationEvent) -> Self {
        Self {
            subject: payload.subject(),
            message_id: MessageId::new(payload.message_id()),
            payload,
        }
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub fn message_id(&self) -> &MessageId {
        &self.message_id
    }

    #[must_use]
    pub fn payload(&self) -> &OperationEvent {
        &self.payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOperationEvent {
    pub sequence: EventSequence,
    pub duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationEventLog {
    jetstream: jetstream::Context,
}

impl AsyncNatsOperationEventLog {
    #[must_use]
    pub fn new(jetstream: jetstream::Context) -> Self {
        Self { jetstream }
    }

    pub async fn append(
        &self,
        append: OperationEventAppend,
    ) -> Result<StoredOperationEvent, OperationEventLogError> {
        let payload =
            serde_json::to_vec(&append.payload).map_err(OperationEventLogError::EncodeEvent)?;
        let publish = PublishMessage::build()
            .payload(payload.into())
            .message_id(append.message_id.as_str());
        let ack_future = with_io_timeout(
            "operation event publish request",
            self.jetstream.send_publish(append.subject, publish),
        )
        .await?
        .map_err(|error| OperationEventLogError::PublishRequest {
            message: error.to_string(),
        })?;
        let ack = with_io_timeout("operation event publish ack", async { ack_future.await })
            .await?
            .map_err(|error| OperationEventLogError::PublishAck {
                message: error.to_string(),
            })?;

        Ok(StoredOperationEvent {
            sequence: EventSequence::try_new(ack.sequence).map_err(|error| {
                OperationEventLogError::InvalidAckSequence {
                    sequence: ack.sequence,
                    error,
                }
            })?,
            duplicate: ack.duplicate,
        })
    }

    pub async fn event_at_sequence(
        &self,
        sequence: EventSequence,
    ) -> Result<OperationEvent, OperationEventLogError> {
        let stream = self.operation_stream().await?;
        let message = with_io_timeout(
            "operation event stream read",
            stream.get_raw_message(sequence.get()),
        )
        .await?
        .map_err(|error| OperationEventLogError::ReadEvent {
            message: error.to_string(),
        })?;

        serde_json::from_slice(&message.payload).map_err(OperationEventLogError::DecodeEvent)
    }

    pub async fn replay_operation(
        &self,
        operation_id: &OperationId,
        start_sequence: EventSequence,
        limit: OperationEventReplayLimit,
    ) -> Result<OperationEventReplayPage, OperationEventReplayReadError> {
        let stream = self.operation_stream_for_replay().await?;
        let subject = op_watch(operation_id);
        let mut next_sequence = start_sequence.get();
        let limit = limit.as_usize();
        let mut events = Vec::with_capacity(limit);

        while events.len() < limit {
            let Some(message) =
                next_replay_message(&stream, subject.as_str(), next_sequence).await?
            else {
                return Ok(OperationEventReplayPage::caught_up(events));
            };
            let replayed = replayed_event_from_replay_message(message.sequence, &message.payload)?;
            events.push(replayed);
            next_sequence = message.sequence.checked_add(1).ok_or(
                OperationEventReplayReadError::InvalidNextReplaySequence {
                    sequence: message.sequence,
                },
            )?;
        }

        match next_replay_message(&stream, subject.as_str(), next_sequence).await? {
            Some(message) => Ok(OperationEventReplayPage::more(
                events,
                event_sequence_from_replay_u64(message.sequence)?,
            )),
            None => Ok(OperationEventReplayPage::caught_up(events)),
        }
    }

    pub async fn event_at_subject(
        &self,
        subject: &str,
    ) -> Result<Option<(StoredOperationEvent, OperationEvent)>, OperationEventLogError> {
        let stream = self.operation_stream().await?;
        let message = match with_io_timeout(
            "operation event subject read",
            stream.get_last_raw_message_by_subject(subject),
        )
        .await?
        {
            Ok(message) => message,
            Err(error) if error.kind() == LastRawMessageErrorKind::NoMessageFound => {
                return Ok(None);
            }
            Err(error) => {
                return Err(OperationEventLogError::ReadEvent {
                    message: error.to_string(),
                });
            }
        };
        let sequence = EventSequence::try_new(message.sequence).map_err(|error| {
            OperationEventLogError::InvalidAckSequence {
                sequence: message.sequence,
                error,
            }
        })?;
        let event = serde_json::from_slice(&message.payload)
            .map_err(OperationEventLogError::DecodeEvent)?;

        Ok(Some((
            StoredOperationEvent {
                sequence,
                duplicate: true,
            },
            event,
        )))
    }

    async fn operation_stream(&self) -> Result<Stream, OperationEventLogError> {
        with_io_timeout(
            "operation event stream lookup",
            self.jetstream.get_stream(PLZ_OPS_STREAM),
        )
        .await?
        .map_err(|error| OperationEventLogError::ReadEvent {
            message: error.to_string(),
        })
    }

    async fn operation_stream_for_replay(&self) -> Result<Stream, OperationEventReplayReadError> {
        with_io_timeout(
            "operation event stream lookup",
            self.jetstream.get_stream(PLZ_OPS_STREAM),
        )
        .await?
        .map_err(|error| OperationEventReplayReadError::ReadEvent {
            message: error.to_string(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OperationEventLogError {
    #[error("encode operation event: {0}")]
    EncodeEvent(serde_json::Error),
    #[error("decode operation event: {0}")]
    DecodeEvent(serde_json::Error),
    #[error("publish operation event: {message}")]
    PublishRequest { message: String },
    #[error("ack operation event publish: {message}")]
    PublishAck { message: String },
    #[error("read operation event: {message}")]
    ReadEvent { message: String },
    #[error("{operation} timed out")]
    Timeout { operation: &'static str },
    #[error("operation event ack sequence {sequence} is invalid: {error}")]
    InvalidAckSequence {
        sequence: u64,
        error: EventSequenceError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum OperationEventReplayReadError {
    #[error("decode operation event: {0}")]
    DecodeEvent(serde_json::Error),
    #[error("read operation event: {message}")]
    ReadEvent { message: String },
    #[error("{operation} timed out")]
    Timeout { operation: &'static str },
    #[error("operation event sequence {sequence} is invalid: {error}")]
    InvalidEventSequence {
        sequence: u64,
        error: EventSequenceError,
    },
    #[error("operation replay next sequence {sequence} is invalid")]
    InvalidNextReplaySequence { sequence: u64 },
}

impl From<NatsIoTimeout> for OperationEventLogError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<NatsIoTimeout> for OperationEventReplayReadError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

async fn next_replay_message(
    stream: &Stream,
    subject: &str,
    sequence: u64,
) -> Result<Option<StreamMessage>, OperationEventReplayReadError> {
    match with_io_timeout(
        "operation event replay read",
        stream
            .raw_message_builder()
            .sequence(sequence)
            .next_by_subject(subject)
            .send(),
    )
    .await?
    {
        Ok(message) => Ok(Some(message)),
        Err(error) if error.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(None),
        Err(error) => Err(OperationEventReplayReadError::ReadEvent {
            message: error.to_string(),
        }),
    }
}

fn replayed_event_from_replay_message(
    sequence: u64,
    payload: &[u8],
) -> Result<ReplayedOperationEvent, OperationEventReplayReadError> {
    let sequence = event_sequence_from_replay_u64(sequence)?;
    let event =
        serde_json::from_slice(payload).map_err(OperationEventReplayReadError::DecodeEvent)?;

    Ok(ReplayedOperationEvent { sequence, event })
}

fn event_sequence_from_replay_u64(
    sequence: u64,
) -> Result<EventSequence, OperationEventReplayReadError> {
    EventSequence::try_new(sequence)
        .map_err(|error| OperationEventReplayReadError::InvalidEventSequence { sequence, error })
}
