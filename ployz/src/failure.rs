use std::{borrow::Cow, io, process::ExitCode};

use ployz_core::{
    CodecError, MachineSelectorError, MachineUpdateError, RpcError, ServiceSelectorError,
    StreamProtocolError, ValueError,
};
use thiserror::Error;

use crate::{
    caddy::CaddyImageError,
    compose::ComposeError,
    connect::{ConnectError, TransportError},
    context::{ConfigError, ConnectionError, ContextError},
    deploy::PlanError,
    dns::{DomainRequired, Error as DnsError, NoReachableMachines},
    image::PushError,
    operator::{ContainerSelectorError, OperatorError},
    provisioning::ProvisionError,
    service::ServiceClientError,
    volume::AssignmentError,
};

/// CLI command outcome. `Display` is product stderr. `Exit` is silent.
#[derive(Debug, Error)]
pub enum Failure {
    #[error("{0}")]
    Command(Box<Cause>),
    #[error("exit {0}")]
    Exit(u8),
}

#[derive(Debug, Error)]
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
    Config(ConfigError),
    #[error(transparent)]
    Connect(ConnectError),
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
    MachineUpdate(MachineUpdateError),
    #[error(transparent)]
    DomainRequired(DomainRequired),
    #[error(transparent)]
    NoReachableMachines(NoReachableMachines),
    #[error(transparent)]
    Protocol(StreamProtocolError),
    #[error(transparent)]
    Push(PushError),
    #[error(transparent)]
    Operator(OperatorError),
    #[error(transparent)]
    Dns(DnsError),
    #[error(transparent)]
    Transport(TransportError),
    #[error("{}", .0.message)]
    Rpc(RpcError),
    #[error(transparent)]
    Codec(CodecError),
    #[error(transparent)]
    Assignment(AssignmentError),
    #[error(transparent)]
    Provision(ProvisionError),
    #[error(transparent)]
    CaddyImage(CaddyImageError),
    #[error("{0}")]
    Io(io::Error),
    #[error("{0}")]
    Json(serde_json::Error),
    #[error("{0}")]
    ParseInt(std::num::ParseIntError),
    #[error("{0}")]
    ShellWords(shell_words::ParseError),
}

impl Failure {
    #[must_use]
    pub fn exit(code: u8) -> Self {
        Self::Exit(code)
    }

    pub fn usage(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Command(Box::new(Cause::Usage(message.into())))
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
        Self::Command(Box::new(cause))
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
            ConnectError::Config(error) => error.into(),
            ConnectError::Attempt(_)
            | ConnectError::Io(_)
            | ConnectError::Dial(_)
            | ConnectError::MissingMachineDetails
            | ConnectError::SshProbe { .. }
            | ConnectError::Routing(_)
            | ConnectError::Join(_)
            | ConnectError::ProxyUnsupported(_)
            | ConnectError::UnsupportedNetwork(_)
            | ConnectError::Path { .. }
            | ConnectError::AllFailed { .. }
            | ConnectError::Rpc(_)
            | ConnectError::Codec(_)
            | ConnectError::Remote(_)
            | ConnectError::Framing(_) => Cause::Connect(error).into(),
        }
    }
}

impl From<ConfigError> for Failure {
    fn from(error: ConfigError) -> Self {
        Cause::Config(error).into()
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Cause::Io(error).into()
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Cause::Json(error).into()
    }
}

impl From<std::num::ParseIntError> for Failure {
    fn from(error: std::num::ParseIntError) -> Self {
        Cause::ParseInt(error).into()
    }
}

impl From<shell_words::ParseError> for Failure {
    fn from(error: shell_words::ParseError) -> Self {
        Cause::ShellWords(error).into()
    }
}

impl From<PushError> for Failure {
    fn from(error: PushError) -> Self {
        Cause::Push(error).into()
    }
}

impl From<OperatorError> for Failure {
    fn from(error: OperatorError) -> Self {
        match error {
            OperatorError::Container(error) => error.into(),
            OperatorError::Protocol(error) => error.into(),
            OperatorError::Connect(error) => error.into(),
            OperatorError::Selector(error) => error.into(),
            OperatorError::MachineSelector(error) => error.into(),
            OperatorError::Value(error) => error.into(),
            OperatorError::Rpc(_)
            | OperatorError::Codec(_)
            | OperatorError::StreamClosed
            | OperatorError::TtyRequiresStdin
            | OperatorError::InvalidServiceSelector(_)
            | OperatorError::InvalidTail(_)
            | OperatorError::InvalidProxyPort
            | OperatorError::InvalidLocalPort(_)
            | OperatorError::InvalidRemotePort(_)
            | OperatorError::NoHealthyContainer
            | OperatorError::NoContainersOnMachines { .. }
            | OperatorError::NoSelectedServices
            | OperatorError::NoMachines
            | OperatorError::SnapshotStale
            | OperatorError::UnsupportedLogService { .. }
            | OperatorError::OpenContainerLogs { .. }
            | OperatorError::OpenMachineLogs { .. } => Cause::Operator(error).into(),
        }
    }
}

impl From<DnsError> for Failure {
    fn from(error: DnsError) -> Self {
        match error {
            DnsError::Connect(error) => error.into(),
            DnsError::NoReachableMachines(error) => error.into(),
            DnsError::Inspect { .. } | DnsError::Http(_) => Cause::Dns(error).into(),
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
        Cause::Transport(error).into()
    }
}

impl From<tonic::Status> for Failure {
    fn from(status: tonic::Status) -> Self {
        TransportError::from(status).into()
    }
}

impl From<RpcError> for Failure {
    fn from(error: RpcError) -> Self {
        Cause::Rpc(error).into()
    }
}

impl From<CodecError> for Failure {
    fn from(error: CodecError) -> Self {
        Cause::Codec(error).into()
    }
}

impl From<AssignmentError> for Failure {
    fn from(error: AssignmentError) -> Self {
        Cause::Assignment(error).into()
    }
}

impl From<ProvisionError> for Failure {
    fn from(error: ProvisionError) -> Self {
        Cause::Provision(error).into()
    }
}

impl From<CaddyImageError> for Failure {
    fn from(error: CaddyImageError) -> Self {
        Cause::CaddyImage(error).into()
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::MachineName;

    use super::*;

    #[test]
    fn missing_config_stays_a_typed_context_error() {
        let failure = Failure::from(ContextError::NoConfig);
        assert!(matches!(
            &failure,
            Failure::Command(cause) if matches!(cause.as_ref(), Cause::Context(ContextError::NoConfig))
        ));
        assert_eq!(
            failure.to_string(),
            "no Ployz config or local daemon socket is available"
        );
        assert!(!matches!(
            failure,
            Failure::Command(cause) if matches!(cause.as_ref(), Cause::Usage(_))
        ));
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
        assert!(matches!(
            Failure::from(ConnectError::Context(ContextError::NoConfig)),
            Failure::Command(cause) if matches!(cause.as_ref(), Cause::Context(ContextError::NoConfig))
        ));
    }

    #[test]
    fn exit_is_not_a_printed_command_failure() {
        assert!(matches!(Failure::exit(7), Failure::Exit(7)));
        assert!(!matches!(
            Failure::exit(1),
            Failure::Command(cause) if matches!(cause.as_ref(), Cause::Usage(_))
        ));
        assert_eq!(terminate(Err(Failure::exit(3))), ExitCode::from(3));
        assert_eq!(terminate(Ok(())), ExitCode::SUCCESS);
        assert_eq!(terminate(Err(Failure::usage("nope"))), ExitCode::FAILURE);
    }
}
