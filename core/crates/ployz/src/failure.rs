use std::{borrow::Cow, error::Error, fmt, io, process::ExitCode};

use ployz_core::{
    CodecError, ContainerSelectorError, DataLoss, MachineSelectorError, MachineUpdateError,
    PartialResult, RpcError, RpcErrorCode, ServiceSelectorError, StreamProtocolError,
    UnconfirmedDataLoss, ValueError,
};
use serde_json::Value;

use crate::{
    cloud_enroll,
    cloud_login::LoginError,
    connect::{ConnectError, TransportError},
    context::{ConfigError, ConnectionError, ContextError},
    deploy::{DeployError, PlanError},
    image::PushError,
    ingress::IngressImageError,
    namespace::NamespaceError,
    operator::OperatorError,
    provisioning::ProvisionError,
};

/// Exit code of a command that printed its result but did not fully succeed.
pub const PARTIAL_EXIT: u8 = 3;

/// Exit code of a rejected command line, as clap exits.
pub const USAGE_EXIT: u8 = 2;

/// CLI command outcome. `Display` is product stderr. `exit` is silent.
#[derive(Debug)]
pub struct Failure {
    inner: Inner,
}

#[derive(Debug)]
enum Inner {
    /// A printed failure and the exit code it ends the process with.
    Command(Box<dyn Error + Send + Sync>, u8),
    Exit(u8),
}

/// A product failure raised by the CLI itself, with its `--json` error code.
// Skip marker for later capture_exception; not a library error.
#[derive(Debug)]
struct Message {
    code: RpcErrorCode,
    text: Cow<'static, str>,
    details: Value,
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Error for Message {}

impl Failure {
    pub(crate) fn command(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            inner: Inner::Command(Box::new(error), 1),
        }
    }

    /// End the process with `code` instead of 1 when this failure is printed.
    #[must_use]
    pub fn with_exit(mut self, code: u8) -> Self {
        if let Inner::Command(_, exit) = &mut self.inner {
            *exit = code;
        }
        self
    }

    #[must_use]
    pub fn exit(code: u8) -> Self {
        Self {
            inner: Inner::Exit(code),
        }
    }

    /// The result is printed, but some targets failed or never answered.
    #[must_use]
    pub fn partial() -> Self {
        Self::exit(PARTIAL_EXIT)
    }

    /// The input was wrong.
    pub fn usage(message: impl Into<Cow<'static, str>>) -> Self {
        Self::coded(RpcErrorCode::InvalidArgument, message)
    }

    /// The named target does not exist.
    pub fn not_found(message: impl Into<Cow<'static, str>>) -> Self {
        Self::coded(RpcErrorCode::NotFound, message)
    }

    /// The selector matches more than one target.
    pub fn ambiguous(message: impl Into<Cow<'static, str>>) -> Self {
        Self::coded(RpcErrorCode::Ambiguous, message)
    }

    /// The target's current state refuses the request.
    pub fn conflict(message: impl Into<Cow<'static, str>>) -> Self {
        Self::coded(RpcErrorCode::Conflict, message)
    }

    /// A target could not be reached or its outcome is unknown.
    pub fn unavailable(message: impl Into<Cow<'static, str>>) -> Self {
        Self::coded(RpcErrorCode::Unavailable, message)
    }

    /// A failure with an explicit `--json` error code.
    pub fn coded(code: RpcErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::detailed(code, message, Value::Null)
    }

    /// A failure whose `--json` error carries machine-readable evidence in `details`.
    pub fn detailed(
        code: RpcErrorCode,
        message: impl Into<Cow<'static, str>>,
        details: Value,
    ) -> Self {
        Self::command(Message {
            code,
            text: message.into(),
            details,
        })
    }

    /// This failure with a new message, keeping its `--json` code.
    pub(crate) fn reworded(&self, message: impl Into<Cow<'static, str>>) -> Self {
        Self::coded(self.report().code, message)
    }

    /// One product line for a follow-on failure. `terminate` prints it once.
    pub fn warned(context: impl fmt::Display, cause: impl fmt::Display) -> Self {
        Self::coded(
            RpcErrorCode::Internal,
            format!("WARNING: {context}: {cause}."),
        )
    }

    /// The `--json` error object: the RPC error shape and vocabulary.
    #[must_use]
    pub fn report(&self) -> RpcError {
        let (code, details) = match &self.inner {
            Inner::Command(error, _) => classify(error.as_ref()),
            Inner::Exit(_) => (RpcErrorCode::Internal, Value::Null),
        };
        RpcError {
            code,
            message: self.to_string(),
            details,
        }
    }
}

fn classify(error: &(dyn Error + Send + Sync + 'static)) -> (RpcErrorCode, Value) {
    if let Some(message) = error.downcast_ref::<Message>() {
        return (message.code.clone(), message.details.clone());
    }
    if let Some(error) = error.downcast_ref::<RpcError>() {
        return (error.code.clone(), error.details.clone());
    }
    if let Some(ConnectError::Remote(error)) = error.downcast_ref::<ConnectError>() {
        return (error.code.clone(), error.details.clone());
    }
    (code(error), Value::Null)
}

/// The `--json` code of a CLI-side error: `invalid_argument` for bad input,
/// `unavailable` when a target could not be reached, `internal` only for real faults.
fn code(error: &(dyn Error + 'static)) -> RpcErrorCode {
    if let Some(error) = error.downcast_ref::<ConnectError>() {
        return connect_code(error);
    }
    if let Some(error) = error.downcast_ref::<RpcError>() {
        return error.code.clone();
    }
    if let Some(error) = error.downcast_ref::<MachineSelectorError>() {
        return machine_selector_code(error);
    }
    if let Some(error) = error.downcast_ref::<ServiceSelectorError>() {
        return match error {
            ServiceSelectorError::NotFound { .. } => RpcErrorCode::NotFound,
            ServiceSelectorError::NameAmbiguity { .. } => RpcErrorCode::Ambiguous,
        };
    }
    if let Some(error) = error.downcast_ref::<ContainerSelectorError>() {
        return match error {
            ContainerSelectorError::NotFound { .. } => RpcErrorCode::NotFound,
            ContainerSelectorError::Ambiguous { .. } => RpcErrorCode::Ambiguous,
        };
    }
    if let Some(error) = error.downcast_ref::<ContextError>() {
        return context_code(error);
    }
    if let Some(error) = error.downcast_ref::<OperatorError>() {
        return operator_code(error);
    }
    if let Some(error) = error.downcast_ref::<PlanError>() {
        return plan_code(error);
    }
    if let Some(error) = error.downcast_ref::<ProvisionError>() {
        return provision_code(error);
    }
    if let Some(error) = error.downcast_ref::<PushError>() {
        return push_code(error);
    }
    if let Some(error) = error.downcast_ref::<CodecError>() {
        return codec_code(error);
    }
    if let Some(error) = error.downcast_ref::<cloud_enroll::Error>() {
        return cloud_enroll_code(error);
    }
    if let Some(error) = error.downcast_ref::<MachineUpdateError>() {
        return match error {
            MachineUpdateError::DuplicateName => RpcErrorCode::Conflict,
            MachineUpdateError::MissingEndpoints => RpcErrorCode::InvalidArgument,
        };
    }
    if let Some(error) = error.downcast_ref::<io::Error>() {
        return io_code(error);
    }
    if error.is::<TransportError>() || error.is::<IngressImageError>() {
        return RpcErrorCode::Unavailable;
    }
    if error.is::<ValueError>()
        || error.is::<ConnectionError>()
        || error.is::<ConfigError>()
        || error.is::<NamespaceError>()
        || error.is::<std::num::ParseIntError>()
        || error.is::<shell_words::ParseError>()
    {
        return RpcErrorCode::InvalidArgument;
    }
    // StreamProtocolError, serde_json::Error, and anything unlisted: a real fault.
    RpcErrorCode::Internal
}

fn connect_code(error: &ConnectError) -> RpcErrorCode {
    match error {
        ConnectError::Remote(error) => error.code.clone(),
        ConnectError::ClientRefused | ConnectError::ClientCleared => RpcErrorCode::Unauthenticated,
        ConnectError::ProxyUnsupported(_) | ConnectError::UnsupportedNetwork(_) => {
            RpcErrorCode::Unsupported
        }
        ConnectError::Context(error) => context_code(error),
        ConnectError::Config(_) | ConnectError::Connection(_) | ConnectError::Value(_) => {
            RpcErrorCode::InvalidArgument
        }
        ConnectError::Codec(error) => codec_code(error),
        // Every connection failed: the last one says why.
        ConnectError::AllFailed {
            last: Some(last), ..
        } => connect_code(last),
        ConnectError::Join(_) => RpcErrorCode::Internal,
        ConnectError::IdentityMismatch { .. } => RpcErrorCode::Unauthenticated,
        ConnectError::Attempt(_)
        | ConnectError::EntryNotReady
        | ConnectError::Io(_)
        | ConnectError::Dial(_)
        | ConnectError::MissingMachineDetails
        | ConnectError::SshClientMissing(_)
        | ConnectError::SshProbe { .. }
        | ConnectError::Routing(_)
        | ConnectError::Path { .. }
        | ConnectError::AllFailed { last: None, .. }
        | ConnectError::Rpc(_)
        | ConnectError::Framing(_) => RpcErrorCode::Unavailable,
    }
}

fn machine_selector_code(error: &MachineSelectorError) -> RpcErrorCode {
    match error {
        MachineSelectorError::NoTargets => RpcErrorCode::InvalidArgument,
        MachineSelectorError::NoVisibleMachines | MachineSelectorError::NotFound(_) => {
            RpcErrorCode::NotFound
        }
        MachineSelectorError::Ambiguous { .. } => RpcErrorCode::Ambiguous,
    }
}

fn operator_code(error: &OperatorError) -> RpcErrorCode {
    match error {
        OperatorError::Connect(error) => connect_code(error),
        OperatorError::Selector(error) => code(error),
        OperatorError::MachineSelector(error) => machine_selector_code(error),
        OperatorError::Container(error) => code(error),
        OperatorError::Codec(error) => codec_code(error),
        OperatorError::OpenContainerLogs { source, .. }
        | OperatorError::OpenMachineLogs { source, .. } => operator_code(source),
        OperatorError::Value(_)
        | OperatorError::TtyRequiresStdin
        | OperatorError::InvalidServiceSelector(_)
        | OperatorError::InvalidTail(_)
        | OperatorError::InvalidLogTime(_)
        | OperatorError::InvalidProxyPort
        | OperatorError::InvalidLocalPort(_)
        | OperatorError::InvalidRemotePort(_)
        | OperatorError::UnsupportedLogService { .. } => RpcErrorCode::InvalidArgument,
        OperatorError::NoRegularContainer
        | OperatorError::NoContainersOnMachines { .. }
        | OperatorError::NoMachines
        | OperatorError::NoServices
        | OperatorError::NoDeploymentContainers => RpcErrorCode::NotFound,
        OperatorError::Rpc(_)
        | OperatorError::StreamClosed
        | OperatorError::NoHealthyContainer
        | OperatorError::SnapshotStale => RpcErrorCode::Unavailable,
        OperatorError::Protocol(_) => RpcErrorCode::Internal,
    }
}

fn plan_code(error: &PlanError) -> RpcErrorCode {
    match error {
        PlanError::Service { source, .. } => plan_code(source),
        PlanError::ConflictingHostPublications { .. }
        | PlanError::ConflictingDockerVolumeDefinitions { .. }
        | PlanError::DuplicateTargetService { .. }
        | PlanError::MixedVolumeModes { .. }
        | PlanError::DependencyCycle { .. } => RpcErrorCode::InvalidArgument,
        PlanError::Storage { .. }
        | PlanError::HostPortConflict { .. }
        | PlanError::InsufficientCapacity
        | PlanError::NoEligibleMachines { .. }
        | PlanError::ServiceModeCannotChange
        | PlanError::ProvisionedVolumeStorageRequired { .. }
        | PlanError::ProvisionedVolumeStorageUnavailable
        | PlanError::ExistingPlainVolume { .. }
        | PlanError::ExistingProvisionedVolumeMismatch { .. }
        | PlanError::HostnameConflict { .. } => RpcErrorCode::Conflict,
        PlanError::CapacityUnknown
        | PlanError::ProvisionedVolumeStorageUnknown { .. }
        | PlanError::DockerVolumeUnavailable { .. } => RpcErrorCode::Unavailable,
    }
}

fn provision_code(error: &ProvisionError) -> RpcErrorCode {
    match error {
        ProvisionError::CleanupAfter { primary, .. } => provision_code(primary),
        ProvisionError::MissingDestination
        | ProvisionError::RemoteTransport(_)
        | ProvisionError::Connection(_)
        | ProvisionError::StorageChoice(_)
        | ProvisionError::ZfsWithoutInstaller => RpcErrorCode::InvalidArgument,
        ProvisionError::NotRoot | ProvisionError::SudoRequired { .. } => {
            RpcErrorCode::Unauthenticated
        }
        ProvisionError::UnsupportedOs | ProvisionError::UnsupportedArchitecture(_) => {
            RpcErrorCode::Unsupported
        }
        ProvisionError::SshClientMissing(_)
        | ProvisionError::Whoami(_)
        | ProvisionError::WhoamiFailed(_)
        | ProvisionError::Sudo(_)
        | ProvisionError::Platform(_)
        | ProvisionError::PlatformFailed(_)
        | ProvisionError::BootstrapDownload { .. }
        | ProvisionError::Transfer(_)
        | ProvisionError::TransferFailed { .. } => RpcErrorCode::Unavailable,
        ProvisionError::WhoamiUtf8
        | ProvisionError::EmptyUser
        | ProvisionError::PlatformUtf8
        | ProvisionError::BootstrapIo { .. }
        | ProvisionError::BootstrapCommand { .. }
        | ProvisionError::BootstrapVerification(_)
        | ProvisionError::Install(_)
        | ProvisionError::InstallFailed { .. }
        | ProvisionError::Cleanup(_)
        | ProvisionError::CleanupFailed { .. }
        | ProvisionError::StorageInput(_) => RpcErrorCode::Internal,
    }
}

fn push_code(error: &PushError) -> RpcErrorCode {
    match error {
        PushError::VariantUnavailable { .. } => RpcErrorCode::NotFound,
        PushError::BuildIncomplete { .. } => RpcErrorCode::Conflict,
        PushError::InvalidReference { .. } | PushError::InvalidSelector(_) => {
            RpcErrorCode::InvalidArgument
        }
        PushError::Selector(error) => machine_selector_code(error),
        PushError::Cluster(error) => connect_code(error),
        PushError::ImageIngest(error) | PushError::PeerPull(error) => error.code.clone(),
        PushError::UnsupportedImageStore => RpcErrorCode::Unsupported,
        // Ctrl-C or the caller stopped delivery: not a fault; it did not finish and can be retried.
        PushError::Cancelled => RpcErrorCode::Unavailable,
    }
}

fn codec_code(error: &CodecError) -> RpcErrorCode {
    match error {
        CodecError::UnsupportedCommand(_) | CodecError::UnsupportedProtocolMajor { .. } => {
            RpcErrorCode::Unsupported
        }
        CodecError::EncodeJson(_)
        | CodecError::DecodeJson(_)
        | CodecError::UnexpectedResponse { .. }
        | CodecError::UnexpectedRequest { .. } => RpcErrorCode::Internal,
    }
}

fn cloud_enroll_code(error: &cloud_enroll::Error) -> RpcErrorCode {
    match error {
        cloud_enroll::Error::Timeout(_)
        | cloud_enroll::Error::Connect(_)
        | cloud_enroll::Error::Http(_)
        | cloud_enroll::Error::RetrySameCommand { .. } => RpcErrorCode::Unavailable,
        cloud_enroll::Error::Json(_) => RpcErrorCode::Internal,
        cloud_enroll::Error::Status { status, .. } => http_status_code(*status),
    }
}

/// The `--json` code of a Cloud HTTP refusal.
fn http_status_code(status: u16) -> RpcErrorCode {
    match status {
        401 | 403 => RpcErrorCode::Unauthenticated,
        404 => RpcErrorCode::NotFound,
        409 => RpcErrorCode::Conflict,
        408 | 429 => RpcErrorCode::Unavailable,
        400..=499 => RpcErrorCode::InvalidArgument,
        _ => RpcErrorCode::Unavailable,
    }
}

fn io_code(error: &io::Error) -> RpcErrorCode {
    use io::ErrorKind;
    let kind = error.kind();
    if kind == ErrorKind::NotFound {
        RpcErrorCode::NotFound
    } else if kind == ErrorKind::InvalidInput {
        RpcErrorCode::InvalidArgument
    } else if matches!(
        kind,
        ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::TimedOut
    ) {
        RpcErrorCode::Unavailable
    } else {
        RpcErrorCode::Internal
    }
}

fn context_code(error: &ContextError) -> RpcErrorCode {
    match error {
        ContextError::NoConfig
        | ContextError::NoContexts(_)
        | ContextError::ContextNotFound { .. }
        | ContextError::NoConnections { .. } => RpcErrorCode::NotFound,
        ContextError::NoCurrentContext(_) | ContextError::Connection(_) => {
            RpcErrorCode::InvalidArgument
        }
    }
}

pub(crate) fn partial_failure_details<T>(result: &PartialResult<T, RpcError>) -> String {
    result
        .failures
        .iter()
        .map(|failure| format!("{}: {}", failure.machine_id, failure.error.message))
        .chain(
            result
                .omissions
                .iter()
                .map(|machine_id| format!("{machine_id}: no terminal response")),
        )
        .collect::<Vec<_>>()
        .join("; ")
}

pub(crate) fn pass_data_loss_names_message(missing: &[DataLoss]) -> String {
    format!(
        "Additional volume loss is not covered by the confirmation: {}. Rerun to review the updated volume list.",
        missing
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(crate) fn refusal_from_rpc(error: RpcError) -> Failure {
    match UnconfirmedDataLoss::from_rpc_error(&error) {
        Some(unconfirmed) => Failure::usage(pass_data_loss_names_message(&unconfirmed.missing)),
        None => error.into(),
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            Inner::Command(error, _) => error.fmt(f),
            Inner::Exit(code) => write!(f, "exit {code}"),
        }
    }
}

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.inner {
            Inner::Command(error, _) => Some(error.as_ref()),
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
        // A printed result stays the one stdout object; what failed after it is partial.
        Err(error) if crate::output::emitted() => {
            eprintln!("{error}");
            ExitCode::from(PARTIAL_EXIT)
        }
        Err(error) => {
            if crate::output::json() {
                crate::output::error(&error.report());
            } else {
                eprintln!("{error}");
            }
            match error.inner {
                Inner::Command(_, exit) | Inner::Exit(exit) => ExitCode::from(exit),
            }
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
    IngressImageError,
    RpcError,
    NamespaceError,
    cloud_enroll::Error,
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

impl From<DeployError> for Failure {
    fn from(error: DeployError) -> Self {
        match error {
            DeployError::Connect(error) => error.into(),
            DeployError::Plan(error) => error.into(),
            DeployError::Namespace(error) => error.into(),
        }
    }
}

impl From<LoginError> for Failure {
    /// Sign-in failures carry the command that fixes them as `details.next`.
    fn from(error: LoginError) -> Self {
        let (code, next) = match &error {
            LoginError::Unreachable { .. } => (RpcErrorCode::Unavailable, None),
            LoginError::Unsupported(_) => (RpcErrorCode::Unsupported, None),
            LoginError::Status { status, .. } => (http_status_code(*status), None),
            LoginError::Reply(_) | LoginError::Store { .. } => (RpcErrorCode::Internal, None),
            LoginError::Corrupt { .. } => (RpcErrorCode::Internal, Some("ployz logout")),
            LoginError::OtherCloud { .. } => (RpcErrorCode::Conflict, Some("ployz logout")),
            LoginError::SignedOut
            | LoginError::Expired
            | LoginError::Denied
            | LoginError::Ended => (RpcErrorCode::Unauthenticated, Some("ployz login")),
            LoginError::AwaitingApproval { .. } => {
                (RpcErrorCode::Unauthenticated, Some("ployz login --wait"))
            }
            LoginError::TokenRefused => (RpcErrorCode::Unauthenticated, Some("ployz token new")),
            LoginError::NotMember(_) => (RpcErrorCode::Unauthenticated, Some("ployz org ls")),
            LoginError::TokenBound | LoginError::NoBilling(_) => (RpcErrorCode::Unsupported, None),
            LoginError::UnknownOrganization(_) => (RpcErrorCode::NotFound, Some("ployz org ls")),
            LoginError::UnknownCredential(_) => (RpcErrorCode::NotFound, Some("ployz token ls")),
            LoginError::AlreadyPro => (RpcErrorCode::Conflict, Some("ployz billing manage")),
        };
        let details = next.map_or(Value::Null, |next| serde_json::json!({ "next": next }));
        Self::detailed(code, error.to_string(), details)
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
    fn exhausted_connections_print_how_many_were_tried() {
        let failure = Failure::from(ConnectError::AllFailed {
            source: crate::context::ConnectionSource::Context("prod".into()),
            attempts: 3,
            setup_retryable: true,
            last: Some(Box::new(ConnectError::Io(io::Error::from(
                io::ErrorKind::ConnectionRefused,
            )))),
        });
        let display = failure.to_string();
        assert!(
            display.contains("3 connections from context prod"),
            "{display}"
        );
        assert!(
            !display.contains("Os {") && !display.contains("code: 111"),
            "{display}"
        );
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
        assert_eq!(source::<Message>(&failure).to_string(), "nope");
        assert_eq!(terminate(Err(failure)), ExitCode::FAILURE);
    }

    #[test]
    fn warned_follow_on_is_one_line_and_fails() {
        let cause = "write context file: permission denied";
        let remove = Failure::warned("local context cleanup failed after Machine removal", cause);
        assert_eq!(
            remove.to_string(),
            "WARNING: local context cleanup failed after Machine removal: write context file: permission denied."
        );
        assert_eq!(remove.to_string().matches(cause).count(), 1);
        assert_eq!(terminate(Err(remove)), ExitCode::FAILURE);
    }

    #[test]
    fn missing_root_is_unauthenticated() {
        let failure = Failure::from(ProvisionError::NotRoot);
        assert_eq!(failure.report().code, RpcErrorCode::Unauthenticated);
    }

    #[test]
    fn cloud_http_status_codes_map_to_their_meaning() {
        for (status, code) in [
            (429, RpcErrorCode::Unavailable),
            (408, RpcErrorCode::Unavailable),
            (404, RpcErrorCode::NotFound),
            (409, RpcErrorCode::Conflict),
            (403, RpcErrorCode::Unauthenticated),
            (422, RpcErrorCode::InvalidArgument),
            (503, RpcErrorCode::Unavailable),
        ] {
            let failure = Failure::from(cloud_enroll::Error::Status {
                status,
                body: String::new(),
            });
            assert_eq!(failure.report().code, code, "HTTP {status}");
        }
    }

    #[test]
    fn bad_log_tail_is_invalid_argument() {
        let failure = Failure::from(OperatorError::InvalidTail("bad".into()));
        assert_eq!(failure.report().code, RpcErrorCode::InvalidArgument);
    }

    #[test]
    fn a_failure_after_a_result_is_partial() {
        crate::output::emit(&"done").unwrap();
        assert_eq!(
            terminate(Err(Failure::usage("nope"))),
            ExitCode::from(PARTIAL_EXIT)
        );
    }

    #[test]
    fn exit_is_not_a_printed_command_failure() {
        assert!(StdError::source(&Failure::exit(3)).is_none());
        assert_eq!(terminate(Err(Failure::exit(3))), ExitCode::from(3));
        assert_eq!(terminate(Ok(())), ExitCode::SUCCESS);
        assert_eq!(terminate(Err(Failure::usage("nope"))), ExitCode::FAILURE);
    }
}
