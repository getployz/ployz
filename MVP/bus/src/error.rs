use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::{PrincipalId, QueueName, Subject, SubjectPattern};

pub type Result<T> = std::result::Result<T, BusError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectParseError {
    Empty,
    EmptyToken,
    WildcardInSubject,
    NonTerminalManyWildcard,
}

impl Display for SubjectParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("subject is empty"),
            Self::EmptyToken => f.write_str("subject contains an empty token"),
            Self::WildcardInSubject => f.write_str("concrete subject may not contain wildcards"),
            Self::NonTerminalManyWildcard => f.write_str("multi-token wildcard must be terminal"),
        }
    }
}

impl Error for SubjectParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusError {
    SubjectParse(SubjectParseError),
    UnauthorizedPublish {
        principal: PrincipalId,
        subject: Subject,
    },
    UnauthorizedSubscribe {
        principal: PrincipalId,
        pattern: SubjectPattern,
    },
    UnauthorizedQueue {
        principal: PrincipalId,
        pattern: SubjectPattern,
        queue: QueueName,
    },
    UnauthorizedResponse {
        principal: PrincipalId,
        inbox: Subject,
    },
    UnauthorizedDrain {
        principal: PrincipalId,
    },
    Draining,
    NoResponders {
        subject: String,
    },
    Timeout {
        subject: String,
    },
    IncompleteResponses {
        target: String,
        expected: usize,
        received: usize,
    },
    ResponseClosed {
        inbox: String,
    },
    DuplicateResponse {
        inbox: String,
    },
    HandlerFailed {
        subject: String,
        reason: String,
    },
}

impl Display for BusError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::SubjectParse(error) => write!(f, "{error}"),
            Self::UnauthorizedPublish { principal, subject } => {
                write!(f, "{principal} may not publish to {subject}")
            }
            Self::UnauthorizedSubscribe { principal, pattern } => {
                write!(f, "{principal} may not subscribe to {pattern}")
            }
            Self::UnauthorizedQueue {
                principal,
                pattern,
                queue,
            } => {
                write!(f, "{principal} may not join queue {queue} on {pattern}")
            }
            Self::UnauthorizedResponse { principal, inbox } => {
                write!(f, "{principal} may not respond to {inbox}")
            }
            Self::UnauthorizedDrain { principal } => {
                write!(f, "{principal} may not drain the bus")
            }
            Self::Draining => f.write_str("bus is draining"),
            Self::NoResponders { subject } => write!(f, "no responders for {subject}"),
            Self::Timeout { subject } => write!(f, "request timed out for {subject}"),
            Self::IncompleteResponses {
                target,
                expected,
                received,
            } => write!(
                f,
                "incomplete responses for {target}: expected {expected}, received {received}"
            ),
            Self::ResponseClosed { inbox } => write!(f, "response inbox closed: {inbox}"),
            Self::DuplicateResponse { inbox } => write!(f, "duplicate response for {inbox}"),
            Self::HandlerFailed { subject, reason } => {
                write!(f, "handler failed for {subject}: {reason}")
            }
        }
    }
}

impl Error for BusError {}

impl From<SubjectParseError> for BusError {
    fn from(value: SubjectParseError) -> Self {
        Self::SubjectParse(value)
    }
}
