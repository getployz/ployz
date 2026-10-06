use std::{borrow::Cow, error::Error, fmt, io};

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
    enrollment::local::Error as EnrollmentHistoryError,
    image::PushError,
    ingress::IngressImageError,
    namespace::NamespaceError,
    operator::OperatorError,
    provisioning::ProvisionError,
    ui::{self, Hint},
};

/// CLI command outcome. `Display` is our sentence; `ui::exit` prints it.
#[derive(Debug)]
pub struct Failure {
    inner: Inner,
}

#[derive(Debug)]
enum Inner {
    /// An error to print, with what the reader can do about it.
    Command {
        error: Box<dyn Error + Send + Sync>,
        hints: Vec<Hint>,
    },
    /// The result is printed, but some targets failed or never answered.
    Partial,
    /// `ployz exec` passing the remote command's exit code through.
    Exit(u8),
}

/// A product failure raised by the CLI itself, with its `--json` error code.
// Skip marker for later capture_exception; not a library error.
#[derive(Debug)]
struct Message {
    code: RpcErrorCode,
    text: Cow<'static, str>,
    details: Value,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Error for Message {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

impl Failure {
    pub(crate) fn command(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            inner: Inner::Command {
                error: Box::new(error),
                hints: Vec::new(),
            },
        }
    }

    /// Add a line telling the reader what to do about this failure.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<Option<Hint>>) -> Self {
        if let (Inner::Command { hints, .. }, Some(hint)) = (&mut self.inner, hint.into()) {
            hints.push(hint);
        }
        self
    }

    /// End the process with the remote command's exit code. Only `ployz exec` uses it.
    #[must_use]
    pub fn exit(code: u8) -> Self {
        Self {
            inner: Inner::Exit(code),
        }
    }

    /// The result is printed, but some targets failed or never answered.
    #[must_use]
    pub fn partial() -> Self {
        Self {
            inner: Inner::Partial,
        }
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
            source: None,
        })
    }

    /// Our sentence over `cause`, which prints as its `cause:` line.
    pub fn caused(
        code: RpcErrorCode,
        message: impl Into<Cow<'static, str>>,
        cause: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self::command(Message {
            code,
            text: message.into(),
            details: Value::Null,
            source: Some(Box::new(cause)),
        })
    }

    #[must_use]
    pub(crate) fn context(self, message: impl Into<Cow<'static, str>>) -> Self {
        let RpcError { code, details, .. } = self.report();
        Self::command(Message {
            code,
            text: message.into(),
            details,
            source: Some(Box::new(self)),
        })
    }

    fn own_hints(&self) -> &[Hint] {
        match &self.inner {
            Inner::Command { hints, .. } => hints,
            Inner::Partial | Inner::Exit(_) => &[],
        }
    }

    /// What the reader can do: hints set here, then those an error from the wire
    /// carried that no hint set here replaces.
    #[must_use]
    pub fn hints(&self) -> Vec<Hint> {
        let mut hints = self.own_hints().to_vec();
        if let Inner::Command { error, .. } = &self.inner {
            for hint in Hint::from_details(&classify(error.as_ref()).1) {
                if !hints.iter().any(|own| own.replaces(&hint)) {
                    hints.push(hint);
                }
            }
        }
        hints
    }

    /// The `cause:` lines: each error below our sentence.
    #[must_use]
    pub fn causes(&self) -> Vec<String> {
        match &self.inner {
            Inner::Command { error, .. } => ui::causes(error.as_ref()),
            Inner::Partial | Inner::Exit(_) => Vec::new(),
        }
    }

    /// The exit code of a failure whose output is already printed: a partial
    /// result, or `ployz exec` passing the remote code through.
    #[must_use]
    pub(crate) const fn printed_exit(&self) -> Option<u8> {
        match self.inner {
            Inner::Partial => Some(ui::PARTIAL_EXIT),
            Inner::Exit(code) => Some(code),
            Inner::Command { .. } => None,
        }
    }

    /// The `--json` error object: the RPC error shape and vocabulary.
    #[must_use]
    pub fn report(&self) -> RpcError {
        let (code, mut details) = match &self.inner {
            Inner::Command { error, .. } => classify(error.as_ref()),
            Inner::Partial | Inner::Exit(_) => (RpcErrorCode::Internal, Value::Null),
        };
        Hint::into_details(self.own_hints(), &mut details);
        RpcError {
            code,
            message: self.to_string(),
            details,
            cause: self.causes(),
        }
    }

    pub(crate) fn json(&self) -> Value {
        let report = self.report();
        let cause = serde_json::json!(report.cause);
        let mut error = serde_json::json!(report);
        if let Some(fields) = error.as_object_mut() {
            fields.insert("cause".into(), cause);
        }
        error
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
    if let Some(OperatorError::NotRunning { missing, running }) =
        error.downcast_ref::<OperatorError>()
    {
        return (
            code(missing),
            serde_json::json!({ "valid_children": running }),
        );
    }
    if let Some(
        failed @ ProvisionError::CleanupAfter {
            cleanup, remove, ..
        },
    ) = error.downcast_ref::<ProvisionError>()
    {
        let mut cleanup_lines = vec![cleanup.to_string()];
        cleanup_lines.extend(ui::causes(cleanup.as_ref()));
        return (
            provision_code(failed),
            serde_json::json!({ "next": remove, "cleanup": cleanup_lines }),
        );
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
    if let Some(error) = error.downcast_ref::<LoginError>() {
        return login_code(error);
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
    if let Some(error) = error.downcast_ref::<TransportError>() {
        return error.to_rpc_error().code;
    }
    if error.is::<IngressImageError>() {
        return RpcErrorCode::Unavailable;
    }
    if let Some(error) = error.downcast_ref::<ConfigError>() {
        return config_code(error);
    }
    if let Some(error) = error.downcast_ref::<EnrollmentHistoryError>() {
        return enrollment_history_code(error);
    }
    if let Some(error) = error.downcast_ref::<crate::cluster::RoleWaitError>() {
        return match error {
            crate::cluster::RoleWaitError::Connect(error) => connect_code(error),
            crate::cluster::RoleWaitError::NotObserved(_)
            | crate::cluster::RoleWaitError::Cancelled => RpcErrorCode::Unavailable,
        };
    }
    if error.is::<UnconfirmedDataLoss>() {
        return RpcErrorCode::InvalidArgument;
    }
    if error.is::<ValueError>()
        || error.is::<ConnectionError>()
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
        ConnectError::Rpc(error) => error.to_rpc_error().code,
        ConnectError::ClientRefused | ConnectError::ClientCleared => RpcErrorCode::Unauthenticated,
        ConnectError::ProxyUnsupported(_) | ConnectError::UnsupportedNetwork(_) => {
            RpcErrorCode::Unsupported
        }
        ConnectError::Context(error) => context_code(error),
        ConnectError::Config(error) => config_code(error),
        ConnectError::Connection(_) | ConnectError::Value(_) => RpcErrorCode::InvalidArgument,
        ConnectError::Codec(error) => codec_code(error),
        // Every connection failed: the last one says why.
        ConnectError::AllFailed {
            last: Some(last), ..
        } => connect_code(last),
        ConnectError::Join(_) => RpcErrorCode::Internal,
        ConnectError::IdentityMismatch { .. } => RpcErrorCode::Unauthenticated,
        ConnectError::Attempt(_)
        | ConnectError::Exhausted(_)
        | ConnectError::EntryNotReady
        | ConnectError::Io(_)
        | ConnectError::Dial(_)
        | ConnectError::MissingMachineDetails
        | ConnectError::SshClientMissing
        | ConnectError::SshProbe { .. }
        | ConnectError::Routing(_)
        | ConnectError::Path { .. }
        | ConnectError::AllFailed { last: None, .. }
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
        OperatorError::Rpc(error) => error.to_rpc_error().code,
        OperatorError::Selector(error) | OperatorError::NotRunning { missing: error, .. } => {
            code(error)
        }
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
        OperatorError::StreamClosed
        | OperatorError::NoServableContainer
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
        ProvisionError::SshClientMissing
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
        | cloud_enroll::Error::RetrySameCommand { .. }
        | cloud_enroll::Error::FounderWait => RpcErrorCode::Unavailable,
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

fn config_code(error: &ConfigError) -> RpcErrorCode {
    match error {
        ConfigError::Context(error) => context_code(error),
        ConfigError::Parse { .. }
        | ConfigError::PrivatePermissions(_)
        | ConfigError::EmptyCurrentContext(_)
        | ConfigError::Read { .. }
        | ConfigError::Write { .. }
        | ConfigError::CreateDirectory { .. }
        | ConfigError::Encode(_)
        | ConfigError::ManagementConnectionMissing => RpcErrorCode::Internal,
    }
}

fn enrollment_history_code(error: &EnrollmentHistoryError) -> RpcErrorCode {
    match error {
        EnrollmentHistoryError::Allocation(_) => RpcErrorCode::Conflict,
        EnrollmentHistoryError::Io(_)
        | EnrollmentHistoryError::Serialization(_)
        | EnrollmentHistoryError::MissingScope => RpcErrorCode::Internal,
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
        .map(|failure| format!("{}: {}", failure.machine_id, crate::ui::row(&failure.error)))
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
            Inner::Command { error, .. } => error.fmt(f),
            Inner::Partial => f.write_str("partial result"),
            Inner::Exit(code) => write!(f, "exit {code}"),
        }
    }
}

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.inner {
            Inner::Command { error, .. } => Some(error.as_ref()),
            Inner::Partial | Inner::Exit(_) => None,
        }
    }
}

/// Error lines kept as a source chain, when the errors themselves can't be kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinedChain {
    line: String,
    next: Option<Box<JoinedChain>>,
}

impl JoinedChain {
    pub(crate) fn new(first: &(dyn Error + 'static), then: &(dyn Error + 'static)) -> Self {
        let mut lines = Vec::new();
        for error in [first, then] {
            lines.push(error.to_string());
            lines.extend(ui::causes(error));
        }
        Self::of(lines).expect("each error gives a line")
    }

    /// A chain of `lines`, outermost first; `None` when there are none.
    pub(crate) fn of(lines: Vec<String>) -> Option<Self> {
        lines.into_iter().rev().fold(None, |next, line| {
            Some(Self {
                line,
                next: next.map(Box::new),
            })
        })
    }
}

impl fmt::Display for JoinedChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.line)
    }
}

impl Error for JoinedChain {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.next
            .as_deref()
            .map(|next| next as &(dyn Error + 'static))
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
    EnrollmentHistoryError,
    crate::cluster::RoleWaitError,
    UnconfirmedDataLoss,
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
    /// Sign-in failures name the command that fixes them.
    fn from(error: LoginError) -> Self {
        let next = match &error {
            LoginError::Corrupt { .. } | LoginError::OtherCloud { .. } => Some("ployz logout"),
            LoginError::SignedOut
            | LoginError::Expired
            | LoginError::Denied
            | LoginError::Ended => Some("ployz login"),
            LoginError::AwaitingApproval { .. } => Some("ployz login --wait"),
            LoginError::TokenRefused => Some("ployz token new"),
            LoginError::NotMember(_) | LoginError::UnknownOrganization(_) => Some("ployz org ls"),
            LoginError::UnknownCredential(_) => Some("ployz token ls"),
            LoginError::AlreadyPro => Some("ployz billing manage"),
            LoginError::Unreachable { .. }
            | LoginError::Unsupported(_)
            | LoginError::Status { .. }
            | LoginError::Reply(_)
            | LoginError::Store { .. }
            | LoginError::TokenBound
            | LoginError::NoBilling(_) => None,
        };
        Self::command(error).hint(next.map(|next| Hint::Next(next.into())))
    }
}

fn login_code(error: &LoginError) -> RpcErrorCode {
    match error {
        LoginError::Unreachable { .. } => RpcErrorCode::Unavailable,
        LoginError::Unsupported(_) | LoginError::NoBilling(_) => RpcErrorCode::Unsupported,
        LoginError::TokenBound => RpcErrorCode::InvalidArgument,
        LoginError::Status { status, .. } => http_status_code(*status),
        LoginError::Reply(_) | LoginError::Store { .. } | LoginError::Corrupt { .. } => {
            RpcErrorCode::Internal
        }
        LoginError::OtherCloud { .. } | LoginError::AlreadyPro => RpcErrorCode::Conflict,
        LoginError::SignedOut
        | LoginError::Expired
        | LoginError::Denied
        | LoginError::Ended
        | LoginError::AwaitingApproval { .. }
        | LoginError::TokenRefused
        | LoginError::NotMember(_) => RpcErrorCode::Unauthenticated,
        LoginError::UnknownOrganization(_) | LoginError::UnknownCredential(_) => {
            RpcErrorCode::NotFound
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

    #[test]
    fn context_keeps_the_inner_details_and_next() {
        let inner = Failure::from(RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Machine is starting".into(),
            details: serde_json::json!({ "next": "ployz server ls", "machine": "alpha" }),
            cause: Vec::new(),
        });
        let outer = inner.context("Could not deploy.");
        let report = outer.report();
        assert_eq!(report.code, RpcErrorCode::Unavailable);
        assert_eq!(report.details.get("next").unwrap(), "ployz server ls");
        assert_eq!(report.details.get("machine").unwrap(), "alpha");
        assert_eq!(outer.hints(), [Hint::Next("ployz server ls".into())]);
        assert_eq!(outer.causes(), ["Machine is starting"]);
    }

    #[test]
    fn a_failure_flattened_to_one_line_keeps_every_cause() {
        let write = ConfigError::Write {
            path: "/etc/ployz/config.yaml".into(),
            source: io::Error::other("disk full"),
        };
        let failure = Failure::caused(RpcErrorCode::Internal, "Could not save the context.", write);
        assert_eq!(
            crate::ui::chain_text(&failure),
            "Could not save the context: Could not write Ployz config /etc/ployz/config.yaml: disk full"
        );
    }

    #[test]
    fn a_config_file_problem_is_never_the_command_line_however_it_is_wrapped() {
        let errors = || {
            let io = || io::Error::other("disk full");
            [
                ConfigError::Read {
                    path: "c".into(),
                    source: io(),
                },
                ConfigError::Write {
                    path: "c".into(),
                    source: io(),
                },
                ConfigError::CreateDirectory {
                    path: "c".into(),
                    source: io(),
                },
                ConfigError::Parse {
                    path: "c".into(),
                    line: Some(1),
                },
                ConfigError::PrivatePermissions("c".into()),
                ConfigError::EmptyCurrentContext("c".into()),
            ]
        };
        for error in errors() {
            assert_eq!(Failure::from(error).report().code, RpcErrorCode::Internal);
        }
        for error in errors() {
            let wrapped = Failure::from(ConnectError::Config(error));
            assert_eq!(wrapped.report().code, RpcErrorCode::Internal);
        }
    }

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
    }

    #[test]
    fn exhausted_connections_print_how_many_were_tried() {
        let failure = Failure::from(ConnectError::AllFailed {
            selection: crate::context::ConnectionSource::Context("prod".into()),
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
            cause: Vec::new(),
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
    fn machine_rpc_refusals_keep_their_codes_through_cli_wrappers() {
        for (status, expected) in [
            (tonic::Code::InvalidArgument, RpcErrorCode::InvalidArgument),
            (tonic::Code::NotFound, RpcErrorCode::NotFound),
            (tonic::Code::Unauthenticated, RpcErrorCode::Unauthenticated),
            (tonic::Code::Unimplemented, RpcErrorCode::Unsupported),
            (tonic::Code::Unavailable, RpcErrorCode::Unavailable),
        ] {
            let transport = || TransportError::from(tonic::Status::new(status, "refused"));
            let failures = [
                Failure::from(transport()),
                Failure::from(ConnectError::Rpc(transport())),
                Failure::from(OperatorError::Rpc(transport())),
                Failure::from(OperatorError::OpenContainerLogs {
                    machine_id: ployz_core::MachineId::parse("1".repeat(32)).unwrap(),
                    container_id: ployz_core::ContainerId::parse("2".repeat(64)).unwrap(),
                    source: Box::new(OperatorError::Rpc(transport())),
                }),
            ];
            for failure in failures {
                assert_eq!(failure.report().code, expected, "{status:?}: {failure}");
            }
        }
    }

    #[test]
    fn printed_exits_are_not_printed_command_failures() {
        assert!(StdError::source(&Failure::exit(7)).is_none());
        assert!(StdError::source(&Failure::partial()).is_none());
        assert_eq!(Failure::exit(7).printed_exit(), Some(7));
        assert_eq!(Failure::partial().printed_exit(), Some(3));
        assert_eq!(Failure::usage("nope").printed_exit(), None);
    }

    #[test]
    fn a_service_that_is_not_running_lists_the_running_ones() {
        let failure = Failure::from(OperatorError::NotRunning {
            missing: ployz_core::ServiceSelectorError::NotFound {
                selector: ployz_core::ServiceSelector::parse("nope").unwrap(),
            },
            running: vec!["web".into()],
        });
        assert_eq!(failure.report().code, RpcErrorCode::NotFound);
        assert_eq!(failure.hints(), [Hint::valid(["web"])]);
    }

    #[test]
    fn context_keeps_the_code_and_hints_and_chains_the_cause() {
        let failure = Failure::not_found("No Service nope.")
            .hint(Hint::valid(["web"]))
            .context("Server initialized; startup incomplete.");
        assert_eq!(failure.report().code, RpcErrorCode::NotFound);
        assert_eq!(failure.hints(), [Hint::valid(["web"])]);
        assert_eq!(failure.causes(), ["No Service nope."]);
    }
}
