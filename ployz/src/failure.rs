use std::{borrow::Cow, fmt, io, process::ExitCode};

use ployz_core::{
    CodecError, MachineSelectorError, MachineUpdateError, RpcError, ServiceSelectorError,
    StreamProtocolError, ValueError,
};

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

/// CLI command outcome. `Display` is product stderr. `exit` is silent.
#[derive(Debug)]
pub struct Failure {
    inner: Inner,
}

#[derive(Debug)]
enum Inner {
    Stderr(Cow<'static, str>),
    Exit(u8),
}

impl Failure {
    fn stderr(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            inner: Inner::Stderr(message.into()),
        }
    }

    #[must_use]
    pub fn exit(code: u8) -> Self {
        Self {
            inner: Inner::Exit(code),
        }
    }

    pub fn usage(message: impl Into<Cow<'static, str>>) -> Self {
        Self::stderr(message)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            Inner::Stderr(message) => f.write_str(message),
            Inner::Exit(code) => write!(f, "exit {code}"),
        }
    }
}

#[must_use]
pub fn terminate(result: Result<(), Failure>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure {
            inner: Inner::Exit(code),
        }) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

impl From<ValueError> for Failure {
    fn from(error: ValueError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<ContextError> for Failure {
    fn from(error: ContextError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<ConnectionError> for Failure {
    fn from(error: ConnectionError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<MachineSelectorError> for Failure {
    fn from(error: MachineSelectorError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<ServiceSelectorError> for Failure {
    fn from(error: ServiceSelectorError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<ContainerSelectorError> for Failure {
    fn from(error: ContainerSelectorError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<PlanError> for Failure {
    fn from(error: PlanError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<ComposeError> for Failure {
    fn from(error: ComposeError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<MachineUpdateError> for Failure {
    fn from(error: MachineUpdateError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<DomainRequired> for Failure {
    fn from(error: DomainRequired) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<NoReachableMachines> for Failure {
    fn from(error: NoReachableMachines) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<StreamProtocolError> for Failure {
    fn from(error: StreamProtocolError) -> Self {
        Self::stderr(error.to_string())
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
            | ConnectError::Framing(_) => Self::stderr(error.to_string()),
        }
    }
}

impl From<ConfigError> for Failure {
    fn from(error: ConfigError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<std::num::ParseIntError> for Failure {
    fn from(error: std::num::ParseIntError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<shell_words::ParseError> for Failure {
    fn from(error: shell_words::ParseError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<PushError> for Failure {
    fn from(error: PushError) -> Self {
        Self::stderr(error.to_string())
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
            | OperatorError::OpenMachineLogs { .. } => Self::stderr(error.to_string()),
        }
    }
}

impl From<DnsError> for Failure {
    fn from(error: DnsError) -> Self {
        match error {
            DnsError::Connect(error) => error.into(),
            DnsError::NoReachableMachines(error) => error.into(),
            DnsError::Inspect { .. } | DnsError::Http(_) => Self::stderr(error.to_string()),
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
        Self::stderr(error.to_string())
    }
}

impl From<tonic::Status> for Failure {
    fn from(status: tonic::Status) -> Self {
        TransportError::from(status).into()
    }
}

impl From<RpcError> for Failure {
    fn from(error: RpcError) -> Self {
        Self::stderr(error.message)
    }
}

impl From<CodecError> for Failure {
    fn from(error: CodecError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<AssignmentError> for Failure {
    fn from(error: AssignmentError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<ProvisionError> for Failure {
    fn from(error: ProvisionError) -> Self {
        Self::stderr(error.to_string())
    }
}

impl From<CaddyImageError> for Failure {
    fn from(error: CaddyImageError) -> Self {
        Self::stderr(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::MachineName;

    use super::*;

    #[test]
    fn missing_config_prints_the_no_config_string() {
        let failure = Failure::from(ContextError::NoConfig);
        assert_eq!(
            failure.to_string(),
            "no Ployz config or local daemon socket is available"
        );
        assert_eq!(terminate(Err(failure)), ExitCode::FAILURE);
    }

    #[test]
    fn invalid_machine_name_display_is_stable() {
        let error = MachineName::parse("BAD NAME").unwrap_err();
        let failure = Failure::from(error);
        assert_eq!(
            failure.to_string(),
            "invalid Machine Name \"BAD NAME\": a 1-63 character lowercase DNS label"
        );
        assert_eq!(terminate(Err(failure)), ExitCode::FAILURE);
    }

    #[test]
    fn connect_context_errors_unwrap_to_context() {
        let from_connect = Failure::from(ConnectError::Context(ContextError::NoConfig));
        let from_context = Failure::from(ContextError::NoConfig);
        assert_eq!(from_connect.to_string(), from_context.to_string());
        assert_eq!(
            from_connect.to_string(),
            "no Ployz config or local daemon socket is available"
        );
        assert_eq!(terminate(Err(from_connect)), ExitCode::FAILURE);
    }

    #[test]
    fn exit_is_not_a_printed_command_failure() {
        assert_eq!(Failure::usage("nope").to_string(), "nope");
        assert_eq!(terminate(Err(Failure::exit(3))), ExitCode::from(3));
        assert_eq!(terminate(Ok(())), ExitCode::SUCCESS);
        assert_eq!(terminate(Err(Failure::usage("nope"))), ExitCode::FAILURE);
    }
}
