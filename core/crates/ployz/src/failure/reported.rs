//! The list of every error the CLI reports with its own code. It generates their `From`
//! impls and the downcast `classify` walks, so a listed type cannot go unclassified. An
//! error outside it reaches `Failure::command` as a fault, `internal` without details.

use std::{error::Error, io};

use ployz_core::{
    CodecError, ContainerSelectorError, Hint, MachineSelectorError, MachineUpdateError, RpcError,
    RpcErrorCode, ServiceSelectorError, StreamProtocolError, UnconfirmedDataLoss, ValueError,
};
use serde_json::Value;

use super::{Failure, Message};
use crate::{
    cloud,
    cloud::login::LoginError,
    coded::Coded,
    connect::{ConnectError, TransportError},
    context::{ConfigError, ConnectionError, ContextError},
    deploy::PlanError,
    enrollment::local::Error as EnrollmentHistoryError,
    image::PushError,
    namespace::NamespaceError,
    operator::{OperatorError, input::InputError},
    provisioning::ProvisionError,
    published::PublishError,
};

fn details_of(error: &impl Coded) -> Value {
    let mut details = error.evidence().unwrap_or(Value::Null);
    Hint::into_details(&error.hints(), &mut details);
    details
}

macro_rules! reported_errors {
    (
        converted: [$($converted:ty),+ $(,)?],
        by_hand: [$($by_hand:ty),+ $(,)?] $(,)?
    ) => {
        $(impl From<$converted> for Failure {
            fn from(error: $converted) -> Self {
                Self::command(error)
            }
        })+

        /// The code and details of `error` if it is one of the listed types.
        pub(super) fn reported(error: &(dyn Error + 'static)) -> Option<(RpcErrorCode, Value)> {
            $(if let Some(error) = error.downcast_ref::<$converted>() {
                return Some((error.code(), details_of(error)));
            })+
            $(if let Some(error) = error.downcast_ref::<$by_hand>() {
                return Some((error.code(), details_of(error)));
            })+
            None
        }
    };
}

// `converted` types become a `Failure` as they are; `by_hand` types have a `From`
// beside `Failure` that peels what its wrappers add to the sentence first.
reported_errors! {
    converted: [
        ValueError,
        ContextError,
        ConnectionError,
        MachineSelectorError,
        ServiceSelectorError,
        ContainerSelectorError,
        PlanError,
        MachineUpdateError,
        StreamProtocolError,
        ConfigError,
        io::Error,
        serde_json::Error,
        std::num::ParseIntError,
        shell_words::ParseError,
        PushError,
        TransportError,
        CodecError,
        ProvisionError,
        RpcError,
        NamespaceError,
        cloud::enroll::Error,
        EnrollmentHistoryError,
        crate::cluster::RoleWaitError,
        UnconfirmedDataLoss,
        InputError,
    ],
    by_hand: [ConnectError, OperatorError, LoginError, PublishError, Message],
}
