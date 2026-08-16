use std::{borrow::Cow, error::Error, fmt, io, process::ExitCode};

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
    Command(Box<dyn Error + Send + Sync>),
    Exit(u8),
}

// Skip marker for later capture_exception; not a library error.
#[derive(Debug)]
struct Usage(Cow<'static, str>);

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Usage {}

impl Failure {
    fn command(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            inner: Inner::Command(Box::new(error)),
        }
    }

    #[must_use]
    pub fn exit(code: u8) -> Self {
        Self {
            inner: Inner::Exit(code),
        }
    }

    pub fn usage(message: impl Into<Cow<'static, str>>) -> Self {
        Self::command(Usage(message.into()))
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            Inner::Command(error) => error.fmt(f),
            Inner::Exit(code) => write!(f, "exit {code}"),
        }
    }
}

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.inner {
            Inner::Command(error) => Some(error.as_ref()),
            Inner::Exit(_) => None,
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

macro_rules! from_error {
    ($($t:ty),+ $(,)?) => {
        $(impl From<$t> for Failure {
            fn from(error: $t) -> Self {
                Self::command(error)
            }
        })+
    };
}

from_error!(
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
    RpcError,
);

impl From<ConnectError> for Failure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "opaque Failure peels Display-changing wrappers; the rest keep the original error"
    )]
    fn from(error: ConnectError) -> Self {
        match error {
            ConnectError::Context(error) => error.into(),
            ConnectError::Value(error) => error.into(),
            error => Self::command(error),
        }
    }
}

impl From<OperatorError> for Failure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "opaque Failure peels Display-changing wrappers; the rest keep the original error"
    )]
    fn from(error: OperatorError) -> Self {
        match error {
            OperatorError::Connect(error) => error.into(),
            OperatorError::Protocol(error) => error.into(),
            error => Self::command(error),
        }
    }
}

impl From<DnsError> for Failure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "opaque Failure peels ConnectError; the rest keep the original error"
    )]
    fn from(error: DnsError) -> Self {
        match error {
            DnsError::Connect(error) => error.into(),
            error => Self::command(error),
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

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use ployz_core::{MachineName, RpcError, RpcErrorCode};
    use serde_json::Value;

    use super::*;

    fn source<E: StdError + 'static>(failure: &Failure) -> &E {
        StdError::source(failure)
            .and_then(|error| error.downcast_ref())
            .expect("command Failure should keep the original error")
    }

    #[test]
    fn missing_config_prints_the_no_config_string() {
        let failure = Failure::from(ContextError::NoConfig);
        assert_eq!(
            failure.to_string(),
            "no Ployz config or local daemon socket is available"
        );
        assert!(matches!(
            source::<ContextError>(&failure),
            ContextError::NoConfig
        ));
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
        assert_eq!(
            source::<ValueError>(&failure).to_string(),
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
        assert!(matches!(
            source::<ContextError>(&from_connect),
            ContextError::NoConfig
        ));
        assert!(
            StdError::source(&from_connect)
                .unwrap()
                .downcast_ref::<ConnectError>()
                .is_none()
        );
        assert_eq!(terminate(Err(from_connect)), ExitCode::FAILURE);
    }

    #[test]
    fn connect_keeps_non_peeled_connect_error() {
        let failure = Failure::from(ConnectError::MissingMachineDetails);
        assert_eq!(
            failure.to_string(),
            "connection attempt failed: inspect response omitted Machine details"
        );
        assert!(matches!(
            source::<ConnectError>(&failure),
            ConnectError::MissingMachineDetails
        ));
    }

    #[test]
    fn rpc_error_keeps_the_rpc_error() {
        let failure = Failure::from(RpcError {
            code: RpcErrorCode::Internal,
            message: "boom".into(),
            details: Value::Null,
        });
        assert_eq!(failure.to_string(), "boom");
        assert_eq!(source::<RpcError>(&failure).message, "boom");
        assert_eq!(source::<RpcError>(&failure).code, RpcErrorCode::Internal);
    }

    #[test]
    fn usage_is_not_a_library_error() {
        let failure = Failure::usage("nope");
        assert_eq!(failure.to_string(), "nope");
        assert_eq!(source::<Usage>(&failure).to_string(), "nope");
        assert_eq!(terminate(Err(failure)), ExitCode::FAILURE);
    }

    #[test]
    fn exit_is_not_a_printed_command_failure() {
        assert!(StdError::source(&Failure::exit(3)).is_none());
        assert_eq!(terminate(Err(Failure::exit(3))), ExitCode::from(3));
        assert_eq!(terminate(Ok(())), ExitCode::SUCCESS);
        assert_eq!(terminate(Err(Failure::usage("nope"))), ExitCode::FAILURE);
    }
}
