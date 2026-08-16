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

macro_rules! from_stderr {
    ($($t:ty),+ $(,)?) => {
        $(impl From<$t> for Failure {
            fn from(error: $t) -> Self {
                Self::stderr(error.to_string())
            }
        })+
    };
}

from_stderr!(
    ValueError,
    ContextError,
    ConnectionError,
    MachineSelectorError,
    ServiceSelectorError,
    ContainerSelectorError,
    PlanError,
    ComposeError,
    MachineUpdateError,
    DomainRequired,
    NoReachableMachines,
    StreamProtocolError,
    ConfigError,
    io::Error,
    serde_json::Error,
    std::num::ParseIntError,
    shell_words::ParseError,
    PushError,
    TransportError,
    CodecError,
    AssignmentError,
    ProvisionError,
    CaddyImageError,
);

impl From<ConnectError> for Failure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "opaque Failure peels Display-changing wrappers; the rest stringify"
    )]
    fn from(error: ConnectError) -> Self {
        match error {
            ConnectError::Context(error) => error.into(),
            ConnectError::Value(error) => error.into(),
            error => Self::stderr(error.to_string()),
        }
    }
}

impl From<OperatorError> for Failure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "opaque Failure peels Display-changing wrappers; the rest stringify"
    )]
    fn from(error: OperatorError) -> Self {
        match error {
            OperatorError::Connect(error) => error.into(),
            OperatorError::Protocol(error) => error.into(),
            error => Self::stderr(error.to_string()),
        }
    }
}

impl From<DnsError> for Failure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "opaque Failure peels ConnectError; the rest stringify"
    )]
    fn from(error: DnsError) -> Self {
        match error {
            DnsError::Connect(error) => error.into(),
            error => Self::stderr(error.to_string()),
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
