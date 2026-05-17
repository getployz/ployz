use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::{
    BridgeRuleId, BridgeRuleViolation, FactKey, FactKeyParseError, IslandId, PrincipalId,
    QueueName, RequestTarget, Subject, SubjectPattern,
};

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
pub enum HandlerFailure {
    Application,
    LockPoisoned,
}

impl Display for HandlerFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Application => f.write_str("application error"),
            Self::LockPoisoned => f.write_str("lock poisoned"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorFailure {
    NotRunning,
    Stopped,
    MailboxFull,
    AskTimeout,
    MailboxSendFailed,
    BlockingTaskJoinFailed,
}

impl Display for ActorFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => f.write_str("not running"),
            Self::Stopped => f.write_str("stopped"),
            Self::MailboxFull => f.write_str("mailbox full"),
            Self::AskTimeout => f.write_str("ask timed out"),
            Self::MailboxSendFailed => f.write_str("mailbox send failed"),
            Self::BlockingTaskJoinFailed => f.write_str("blocking task join failed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeFailure {
    Disabled,
}

impl Display for BridgeFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("disabled"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusError {
    SubjectParse(SubjectParseError),
    FactKeyParse(FactKeyParseError),
    UnauthorizedPublish {
        island: IslandId,
        principal: PrincipalId,
        subject: Box<Subject>,
    },
    UnauthorizedRequestTarget {
        island: IslandId,
        principal: PrincipalId,
        target: Box<RequestTarget>,
    },
    UnauthorizedSubscribe {
        island: IslandId,
        principal: PrincipalId,
        pattern: Box<SubjectPattern>,
    },
    UnauthorizedQueue {
        island: IslandId,
        principal: PrincipalId,
        pattern: Box<SubjectPattern>,
        queue: QueueName,
    },
    UnauthorizedResponse {
        island: IslandId,
        principal: PrincipalId,
        inbox: Box<Subject>,
    },
    UnauthorizedDrain {
        island: IslandId,
        principal: PrincipalId,
    },
    UnauthorizedFactWrite {
        island: IslandId,
        principal: PrincipalId,
        key: Box<FactKey>,
    },
    UnauthorizedFactRead {
        island: IslandId,
        principal: PrincipalId,
        key: Box<FactKey>,
    },
    BridgeRuleInvalid {
        violation: BridgeRuleViolation,
    },
    BridgeUnavailable {
        rule_id: BridgeRuleId,
        local_island: IslandId,
        remote_island: IslandId,
        subject: Box<Subject>,
        failure: BridgeFailure,
    },
    BridgeRequestManyUnsupported {
        rule_id: BridgeRuleId,
        local_island: IslandId,
        remote_island: IslandId,
        subject: Box<Subject>,
        requested: usize,
    },
    Draining,
    NoResponders {
        target: Box<RequestTarget>,
    },
    Timeout {
        subject: String,
    },
    IncompleteResponses {
        target: Box<RequestTarget>,
        expected: usize,
        received: usize,
    },
    NoReplyPermit {
        island: IslandId,
        subject: Box<Subject>,
    },
    ResponseClosed {
        inbox: String,
    },
    DeliveryRuntimeStopped,
    DuplicateResponse {
        inbox: String,
    },
    HandlerFailed {
        subject: String,
        failure: HandlerFailure,
    },
    ActorUnavailable {
        actor: String,
        failure: ActorFailure,
    },
}

impl Display for BusError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::SubjectParse(error) => write!(f, "{error}"),
            Self::FactKeyParse(error) => write!(f, "{error}"),
            Self::UnauthorizedPublish {
                island,
                principal,
                subject,
            } => {
                write!(f, "{principal} may not publish to {subject} in {island}")
            }
            Self::UnauthorizedRequestTarget {
                island,
                principal,
                target,
            } => {
                write!(
                    f,
                    "{principal} may not request {} in {island}",
                    target.display()
                )
            }
            Self::UnauthorizedSubscribe {
                island,
                principal,
                pattern,
            } => {
                write!(f, "{principal} may not subscribe to {pattern} in {island}")
            }
            Self::UnauthorizedQueue {
                island,
                principal,
                pattern,
                queue,
            } => {
                write!(
                    f,
                    "{principal} may not join queue {queue} on {pattern} in {island}"
                )
            }
            Self::UnauthorizedResponse {
                island,
                principal,
                inbox,
            } => {
                write!(f, "{principal} may not respond to {inbox} in {island}")
            }
            Self::UnauthorizedDrain { island, principal } => {
                write!(f, "{principal} may not drain the bus in {island}")
            }
            Self::UnauthorizedFactWrite {
                island,
                principal,
                key,
            } => {
                write!(f, "{principal} may not write fact {key} in {island}")
            }
            Self::UnauthorizedFactRead {
                island,
                principal,
                key,
            } => {
                write!(f, "{principal} may not read fact {key} in {island}")
            }
            Self::BridgeRuleInvalid { violation } => {
                write!(f, "invalid bridge rule: {violation}")
            }
            Self::BridgeUnavailable {
                rule_id,
                local_island,
                remote_island,
                subject,
                failure,
            } => write!(
                f,
                "bridge rule {rule_id} from {local_island} to {remote_island} for {subject} is {failure}"
            ),
            Self::BridgeRequestManyUnsupported {
                rule_id,
                local_island,
                remote_island,
                subject,
                requested,
            } => write!(
                f,
                "bridge rule {rule_id} from {local_island} to {remote_island} for {subject} supports one service response, requested {requested}"
            ),
            Self::Draining => f.write_str("bus is draining"),
            Self::NoResponders { target } => {
                write!(f, "no responders for {}", target.display())
            }
            Self::Timeout { subject } => write!(f, "request timed out for {subject}"),
            Self::IncompleteResponses {
                target,
                expected,
                received,
            } => write!(
                f,
                "incomplete responses for {}: expected {expected}, received {received}",
                target.display()
            ),
            Self::NoReplyPermit { island, subject } => {
                write!(
                    f,
                    "message {subject} in {island} does not have a reply permit"
                )
            }
            Self::ResponseClosed { inbox } => write!(f, "response inbox closed: {inbox}"),
            Self::DeliveryRuntimeStopped => f.write_str("delivery runtime stopped"),
            Self::DuplicateResponse { inbox } => write!(f, "duplicate response for {inbox}"),
            Self::HandlerFailed { subject, failure } => {
                write!(f, "handler failed for {subject}: {failure}")
            }
            Self::ActorUnavailable { actor, failure } => {
                write!(f, "actor {actor} unavailable: {failure}")
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

impl From<FactKeyParseError> for BusError {
    fn from(value: FactKeyParseError) -> Self {
        Self::FactKeyParse(value)
    }
}
