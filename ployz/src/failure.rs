use std::{borrow::Cow, io, process::ExitCode};

use ployz_core::{
    MachineSelectorError, MachineUpdateError, ServiceSelectorError, StreamProtocolError, ValueError,
};
use thiserror::Error;

use crate::{
    compose::{ComposeError, ComposePlanError},
    connect::{ConnectError, TransportError},
    context::{ConfigError, ConnectionError, ContextError},
    deploy::PlanError,
    dns::{DomainRequired, Error as DnsError, NoReachableMachines},
    image::PushError,
    operator::{ContainerSelectorError, OperatorError},
    service::ServiceClientError,
};

/// CLI command outcome. `Display` is product stderr. `Exit` is silent.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum Failure {
    #[error("{0}")]
    Command(Cause),
    #[error("exit {0}")]
    Exit(u8),
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum Cause {
    #[error("{0}")]
    Usage(Cow<'static, str>),
    #[error(transparent)]
    Value(ValueError),
    #[error(transparent)]
    Context(ContextError),
    #[error(transparent)]
    Connection(ConnectionError),
    #[error(transparent)]
    MachineSelector(MachineSelectorError),
    #[error(transparent)]
    ServiceSelector(ServiceSelectorError),
    #[error(transparent)]
    ContainerSelector(ContainerSelectorError),
    #[error(transparent)]
    Plan(PlanError),
    #[error(transparent)]
    Compose(ComposeError),
    #[error(transparent)]
    ComposePlan(ComposePlanError),
    #[error(transparent)]
    MachineUpdate(MachineUpdateError),
    #[error(transparent)]
    DomainRequired(DomainRequired),
    #[error(transparent)]
    NoReachableMachines(NoReachableMachines),
    #[error(transparent)]
    Protocol(StreamProtocolError),
}

impl Failure {
    #[must_use]
    pub fn exit(code: u8) -> Self {
        Self::Exit(code)
    }

    pub fn usage(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Command(Cause::Usage(message.into()))
    }
}

#[must_use]
pub fn terminate(result: Result<(), Failure>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Exit(code)) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

impl From<Cause> for Failure {
    fn from(cause: Cause) -> Self {
        Self::Command(cause)
    }
}

impl From<ValueError> for Failure {
    fn from(error: ValueError) -> Self {
        Cause::Value(error).into()
    }
}

impl From<ContextError> for Failure {
    fn from(error: ContextError) -> Self {
        Cause::Context(error).into()
    }
}

impl From<ConnectionError> for Failure {
    fn from(error: ConnectionError) -> Self {
        Cause::Connection(error).into()
    }
}

impl From<MachineSelectorError> for Failure {
    fn from(error: MachineSelectorError) -> Self {
        Cause::MachineSelector(error).into()
    }
}

impl From<ServiceSelectorError> for Failure {
    fn from(error: ServiceSelectorError) -> Self {
        Cause::ServiceSelector(error).into()
    }
}

impl From<ContainerSelectorError> for Failure {
    fn from(error: ContainerSelectorError) -> Self {
        Cause::ContainerSelector(error).into()
    }
}

impl From<PlanError> for Failure {
    fn from(error: PlanError) -> Self {
        Cause::Plan(error).into()
    }
}

impl From<ComposeError> for Failure {
    fn from(error: ComposeError) -> Self {
        Cause::Compose(error).into()
    }
}

impl From<ComposePlanError> for Failure {
    fn from(error: ComposePlanError) -> Self {
        Cause::ComposePlan(error).into()
    }
}

impl From<MachineUpdateError> for Failure {
    fn from(error: MachineUpdateError) -> Self {
        Cause::MachineUpdate(error).into()
    }
}

impl From<DomainRequired> for Failure {
    fn from(error: DomainRequired) -> Self {
        Cause::DomainRequired(error).into()
    }
}

impl From<NoReachableMachines> for Failure {
    fn from(error: NoReachableMachines) -> Self {
        Cause::NoReachableMachines(error).into()
    }
}

impl From<StreamProtocolError> for Failure {
    fn from(error: StreamProtocolError) -> Self {
        Cause::Protocol(error).into()
    }
}

impl From<ConnectError> for Failure {
    fn from(error: ConnectError) -> Self {
        match error {
            ConnectError::Context(error) => error.into(),
            ConnectError::Connection(error) => error.into(),
            ConnectError::Value(error) => error.into(),
            other @ (ConnectError::Attempt(_)
            | ConnectError::ProxyUnsupported(_)
            | ConnectError::UnsupportedNetwork(_)
            | ConnectError::Config(_)
            | ConnectError::Path { .. }
            | ConnectError::AllFailed { .. }
            | ConnectError::Rpc(_)
            | ConnectError::Codec(_)
            | ConnectError::Remote(_)
            | ConnectError::Framing(_)) => Self::usage(other.to_string()),
        }
    }
}

impl From<ConfigError> for Failure {
    fn from(error: ConfigError) -> Self {
        Self::usage(error.to_string())
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::usage(error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::usage(error.to_string())
    }
}

impl From<std::num::ParseIntError> for Failure {
    fn from(error: std::num::ParseIntError) -> Self {
        Self::usage(error.to_string())
    }
}

impl From<shell_words::ParseError> for Failure {
    fn from(error: shell_words::ParseError) -> Self {
        Self::usage(error.to_string())
    }
}

impl From<PushError> for Failure {
    fn from(error: PushError) -> Self {
        Self::usage(error.to_string())
    }
}

impl From<OperatorError> for Failure {
    fn from(error: OperatorError) -> Self {
        match error {
            OperatorError::Container(error) => error.into(),
            OperatorError::Protocol(error) => error.into(),
            other @ (OperatorError::Message(_) | OperatorError::Rpc(_)) => {
                Self::usage(other.to_string())
            }
        }
    }
}

impl From<DnsError> for Failure {
    fn from(error: DnsError) -> Self {
        match error {
            DnsError::Connect(error) => error.into(),
            DnsError::NoReachableMachines(error) => error.into(),
            other @ (DnsError::Inspect { .. } | DnsError::Http(_)) => {
                Self::usage(other.to_string())
            }
        }
    }
}

impl From<ServiceClientError> for Failure {
    fn from(error: ServiceClientError) -> Self {
        match error {
            ServiceClientError::Connect(error) => error.into(),
            ServiceClientError::Selector(error) => error.into(),
        }
    }
}

impl From<TransportError> for Failure {
    fn from(error: TransportError) -> Self {
        Self::usage(error.to_string())
    }
}

impl From<tonic::Status> for Failure {
    fn from(status: tonic::Status) -> Self {
        TransportError::from(status).into()
    }
}

impl From<ployz_core::RpcError> for Failure {
    fn from(error: ployz_core::RpcError) -> Self {
        Self::usage(error.message)
    }
}

impl From<ployz_core::CodecError> for Failure {
    fn from(error: ployz_core::CodecError) -> Self {
        Self::usage(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::MachineName;

    use super::*;

    #[test]
    fn missing_config_stays_a_typed_context_error() {
        let failure = Failure::from(ContextError::NoConfig);
        assert_eq!(failure, Failure::from(ContextError::NoConfig));
        assert_eq!(
            failure.to_string(),
            "no Ployz config or local daemon socket is available"
        );
        assert_ne!(
            Failure::usage("no Ployz config or local daemon socket is available"),
            failure
        );
    }

    #[test]
    fn invalid_machine_name_display_is_stable() {
        let error = MachineName::parse("BAD NAME").unwrap_err();
        assert_eq!(
            Failure::from(error).to_string(),
            "invalid Machine Name \"BAD NAME\": a 1-63 character lowercase DNS label"
        );
    }

    #[test]
    fn connect_context_errors_unwrap_to_context() {
        assert_eq!(
            Failure::from(ConnectError::Context(ContextError::NoConfig)),
            Failure::from(ContextError::NoConfig)
        );
    }

    #[test]
    fn exit_is_not_a_printed_command_failure() {
        assert!(matches!(Failure::exit(7), Failure::Exit(7)));
        assert_ne!(
            Failure::exit(1),
            Failure::usage("remote command exited with status 1")
        );
        assert_eq!(terminate(Err(Failure::exit(3))), ExitCode::from(3));
        assert_eq!(terminate(Ok(())), ExitCode::SUCCESS);
        assert_eq!(terminate(Err(Failure::usage("nope"))), ExitCode::FAILURE);
    }
}
