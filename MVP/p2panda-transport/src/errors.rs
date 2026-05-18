use p2panda_core::Topic;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PandaNetStartupStep {
    AddressBook,
    BootstrapNode,
    Endpoint,
    Discovery,
    Gossip,
    LogSync,
    Stream,
    Publish,
}

#[derive(Debug, Error)]
pub enum PandaNetTransportError {
    #[error("p2panda-net {step:?} timed out after {timeout_ms}ms")]
    Timeout {
        step: PandaNetStartupStep,
        timeout_ms: u64,
    },
    #[error("p2panda-net {step:?} failed: {message}")]
    Startup {
        step: PandaNetStartupStep,
        message: String,
    },
    #[error("p2panda-net quarantine log operation failed: {message}")]
    QuarantineLog { message: String },
    #[error("p2panda-net operation for topic {topic:?} had no body")]
    MissingBody { topic: Topic },
    #[error("p2panda-net stream for topic {topic:?} ended")]
    StreamEnded { topic: Topic },
    #[error("p2panda-net stream for topic {topic:?} lagged: {message}")]
    StreamLagged { topic: Topic, message: String },
    #[error("p2panda-net stream for topic {topic:?} failed: {message}")]
    StreamFailed { topic: Topic, message: String },
    #[error("p2panda-net fact store operation failed: {message}")]
    FactStore { message: String },
    #[error("p2panda-net fact publish had no local operation to send")]
    MissingLocalOperation,
}

impl PandaNetTransportError {
    pub(crate) fn startup(step: PandaNetStartupStep, error: impl ToString) -> Self {
        Self::Startup {
            step,
            message: error.to_string(),
        }
    }
}
