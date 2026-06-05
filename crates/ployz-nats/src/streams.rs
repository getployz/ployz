//! JetStream stream specs and operation event stream harnesses.

use crate::replication::ReplicationFactor;
use ployz_core::ops::{EventSequence, OperationEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSpec {
    pub name: &'static str,
    pub subjects: Vec<String>,
    pub retention: RetentionPolicy,
    pub storage: StorageBackend,
    pub replicas: ReplicationFactor,
    pub discard: DiscardPolicy,
    pub allow_message_schedules: bool,
}

impl StreamSpec {
    #[must_use]
    pub fn new(
        name: &'static str,
        subjects: Vec<String>,
        retention: RetentionPolicy,
        storage: StorageBackend,
        replicas: ReplicationFactor,
        discard: DiscardPolicy,
    ) -> Self {
        Self {
            name,
            subjects,
            retention,
            storage,
            replicas,
            discard,
            allow_message_schedules: false,
        }
    }

    #[must_use]
    pub const fn with_message_schedules(mut self, allow_message_schedules: bool) -> Self {
        self.allow_message_schedules = allow_message_schedules;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPolicy {
    Limits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardPolicy {
    Old,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageId(String);

impl MessageId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStreamMessage {
    pub sequence: EventSequence,
    pub subject: String,
    pub message_id: MessageId,
    pub payload: OperationEvent,
}

impl OperationStreamMessage {
    #[must_use]
    pub fn new(
        sequence: EventSequence,
        subject: impl Into<String>,
        message_id: MessageId,
        payload: OperationEvent,
    ) -> Self {
        Self {
            sequence,
            subject: subject.into(),
            message_id,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEventStream {
    messages: Vec<OperationStreamMessage>,
}

impl OperationEventStream {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    #[must_use]
    pub fn messages(&self) -> &[OperationStreamMessage] {
        &self.messages
    }

    #[must_use]
    pub fn next_sequence(&self) -> EventSequence {
        EventSequence::try_new(self.messages.len() as u64 + 1)
            .expect("operation stream sequence starts at 1")
    }

    pub fn append(
        &mut self,
        subject: impl Into<String>,
        message_id: MessageId,
        payload: OperationEvent,
    ) -> OperationStreamAppend {
        if let Some(existing) = self
            .messages
            .iter()
            .find(|message| message.message_id == message_id)
        {
            return OperationStreamAppend::Duplicate {
                sequence: existing.sequence,
                payload: existing.payload.clone(),
            };
        }

        let sequence = self.next_sequence();
        self.messages.push(OperationStreamMessage::new(
            sequence,
            subject,
            message_id,
            payload.clone(),
        ));

        OperationStreamAppend::Stored { sequence, payload }
    }

    #[must_use]
    pub fn replay(
        &self,
        subject_prefix: &str,
        start_sequence: EventSequence,
    ) -> Vec<OperationStreamMessage> {
        self.messages
            .iter()
            .filter(|message| {
                message.sequence >= start_sequence && message.subject.starts_with(subject_prefix)
            })
            .cloned()
            .collect()
    }
}

impl Default for OperationEventStream {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStreamAppend {
    Stored {
        sequence: EventSequence,
        payload: OperationEvent,
    },
    Duplicate {
        sequence: EventSequence,
        payload: OperationEvent,
    },
}

impl OperationStreamAppend {
    #[must_use]
    pub const fn sequence(&self) -> EventSequence {
        match self {
            Self::Stored { sequence, .. } | Self::Duplicate { sequence, .. } => *sequence,
        }
    }
}
