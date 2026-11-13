//! What `--json` says about each error the CLI reports: its code, the evidence it
//! carries and the hints it gives. One list names every such error: it generates their
//! `From` impls and the downcast `classify` walks, so a listed type cannot go unclassified.
//! An error outside it reaches `Failure::command` as a fault, `internal` without details.

use std::{error::Error, io};

use ployz_core::{
    CodecError, ContainerSelectorError, Hint, MachineSelectorError, MachineUpdateError, RpcError,
    RpcErrorCode, ServiceSelectorError, StreamProtocolError, UnconfirmedDataLoss, ValueError,
    error_chain,
};
use serde_json::Value;

use super::{Failure, Message, http_status_code, io_code, login_code};
use crate::{
    cloud_enroll,
    cloud_login::LoginError,
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

/// An error the CLI turns into a `Failure`.
pub(crate) trait Reported: Error + Send + Sync + 'static {
    /// The `--json` error code.
    fn code(&self) -> RpcErrorCode;

    /// Machine-readable evidence for `details`, beside the keys `hints` write.
    fn evidence(&self) -> Option<Value> {
        None
    }

    /// What the reader can do about it, carried in `details` as hint keys.
    fn hints(&self) -> Vec<Hint> {
        Vec::new()
    }
}

fn details_of(error: &impl Reported) -> Value {
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
        cloud_enroll::Error,
        EnrollmentHistoryError,
        crate::cluster::RoleWaitError,
        UnconfirmedDataLoss,
        InputError,
    ],
    by_hand: [ConnectError, OperatorError, LoginError, PublishError, Message],
}

impl Reported for ConnectError {
    fn code(&self) -> RpcErrorCode {
        match self {
            ConnectError::Remote(error) => error.code.clone(),
            ConnectError::Rpc(error) => error.to_rpc_error().code,
            ConnectError::ClientRefused
            | ConnectError::ClientCleared
            | ConnectError::TunnelNeedsFull => RpcErrorCode::Unauthenticated,
            ConnectError::OlderMachine => RpcErrorCode::Unsupported,
            ConnectError::ProxyUnsupported(_) | ConnectError::UnsupportedNetwork(_) => {
                RpcErrorCode::Unsupported
            }
            ConnectError::Context(error) => error.code(),
            ConnectError::Config(error) => error.code(),
            ConnectError::Connection(_) | ConnectError::Value(_) => RpcErrorCode::InvalidArgument,
            ConnectError::Codec(error) => error.code(),
            // Every connection failed: `last` says why, a Server that needs an upgrade first.
            ConnectError::AllFailed {
                last: Some(last), ..
            } => last.code(),
            ConnectError::Join(_) => RpcErrorCode::Internal,
            ConnectError::IdentityMismatch { .. }
            | ConnectError::ClusterMismatch { .. }
            | ConnectError::ClusterUnverified { .. }
            | ConnectError::PredatesSealing { .. } => RpcErrorCode::Unauthenticated,
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

    fn evidence(&self) -> Option<Value> {
        if let Self::Remote(error) = self {
            return Some(error.details.clone());
        }
        if let Self::Rpc(error) = self {
            return Some(error.to_rpc_error().details);
        }
        None
    }
}

impl Reported for MachineSelectorError {
    fn code(&self) -> RpcErrorCode {
        match self {
            MachineSelectorError::NoTargets => RpcErrorCode::InvalidArgument,
            MachineSelectorError::NoVisibleMachines | MachineSelectorError::NotFound(_) => {
                RpcErrorCode::NotFound
            }
            MachineSelectorError::Ambiguous { .. } => RpcErrorCode::Ambiguous,
        }
    }

    fn hints(&self) -> Vec<Hint> {
        match self {
            Self::NotFound(_) => vec![Hint::Inspect("ployz server ls".into())],
            Self::NoTargets | Self::NoVisibleMachines | Self::Ambiguous { .. } => Vec::new(),
        }
    }
}

impl Reported for InputError {
    fn code(&self) -> RpcErrorCode {
        RpcErrorCode::InvalidArgument
    }
}

impl Reported for OperatorError {
    fn code(&self) -> RpcErrorCode {
        match self {
            OperatorError::Connect(error) => error.code(),
            OperatorError::Rpc(error)
            | OperatorError::StartExec { source: error, .. }
            | OperatorError::ExecConnectionLost { source: error, .. } => error.to_rpc_error().code,
            OperatorError::Selector(error) => error.code(),
            OperatorError::NotRunning { .. } => RpcErrorCode::NotFound,
            OperatorError::MachineSelector(error) => error.code(),
            OperatorError::Container(error) => error.code(),
            OperatorError::Codec(error) => error.code(),
            OperatorError::OpenContainerLogs { source, .. }
            | OperatorError::OpenMachineLogs { source, .. } => source.code(),
            OperatorError::Input(error) => error.code(),
            OperatorError::Value(_) | OperatorError::UnsupportedLogService { .. } => {
                RpcErrorCode::InvalidArgument
            }
            OperatorError::NoRegularContainer
            | OperatorError::NoContainersOnMachines { .. }
            | OperatorError::NoMachines
            | OperatorError::NoServices
            | OperatorError::NoDeploymentContainers => RpcErrorCode::NotFound,
            OperatorError::StreamClosed
            | OperatorError::NotRunningOnAnswered { .. }
            | OperatorError::ExecContainerStopped { .. }
            | OperatorError::NoServableContainer
            | OperatorError::SnapshotStale => RpcErrorCode::Unavailable,
            OperatorError::Protocol(_) => RpcErrorCode::Internal,
        }
    }

    fn evidence(&self) -> Option<Value> {
        if let Self::StartExec { source, .. } = self {
            return Some(source.to_rpc_error().details);
        }
        if let Self::NotRunningOnAnswered { gaps, .. } = self {
            return Some(
                serde_json::json!({ "failures": gaps.failures, "omitted": gaps.omissions }),
            );
        }
        if let Self::Container(container) = self {
            return container.evidence();
        }
        None
    }

    fn hints(&self) -> Vec<Hint> {
        if let Self::Container(container) = self {
            return container.hints();
        }
        if let Self::NotRunning {
            running_in_scope, ..
        } = self
        {
            return vec![Hint::valid(running_in_scope)];
        }
        Vec::new()
    }
}

impl Reported for PlanError {
    fn code(&self) -> RpcErrorCode {
        match self {
            PlanError::Service { source, .. } => source.code(),
            PlanError::ConflictingHostPublications { .. }
            | PlanError::ConflictingDockerVolumeDefinitions { .. }
            | PlanError::DuplicateTargetService { .. }
            | PlanError::MixedVolumeModes { .. }
            | PlanError::DependencyCycle { .. }
            | PlanError::RemovalInTarget { .. }
            | PlanError::RemovalNotSelected { .. }
            | PlanError::RemovalInReservedNamespace { .. } => RpcErrorCode::InvalidArgument,
            PlanError::Storage { .. }
            | PlanError::HostPortConflict { .. }
            | PlanError::InsufficientCapacity
            | PlanError::NoEligibleMachines { .. }
            | PlanError::ServiceModeCannotChange
            | PlanError::ProvisionedVolumeStorageRequired { .. }
            | PlanError::ProvisionedVolumeStorageUnavailable
            | PlanError::ExistingPlainVolume { .. }
            | PlanError::ExistingProvisionedVolumeMismatch { .. }
            | PlanError::NoWriter { .. }
            | PlanError::VolumeServerTakesNoServices { .. }
            | PlanError::HostnameConflict { .. } => RpcErrorCode::Conflict,
            PlanError::CapacityUnknown
            | PlanError::CopiesUnknown { .. }
            | PlanError::VolumeHolderUnknown { .. }
            | PlanError::ProvisionedVolumeStorageUnknown { .. } => RpcErrorCode::Unavailable,
        }
    }
}

impl Reported for ProvisionError {
    fn code(&self) -> RpcErrorCode {
        match self {
            ProvisionError::CleanupAfter { primary, .. } => primary.code(),
            ProvisionError::MissingDestination
            | ProvisionError::RemoteTransport(_)
            | ProvisionError::Connection(_)
            | ProvisionError::ZfsWithoutInstaller => RpcErrorCode::InvalidArgument,
            ProvisionError::NotRoot | ProvisionError::SudoRequired { .. } => {
                RpcErrorCode::Unauthenticated
            }
            ProvisionError::UnsupportedOs
            | ProvisionError::UnsupportedArchitecture(_)
            | ProvisionError::BootstrapUnsigned(_) => RpcErrorCode::Unsupported,
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
            | ProvisionError::BootstrapSignature(_)
            | ProvisionError::Install(_)
            | ProvisionError::InstallFailed { .. }
            | ProvisionError::InstallRefused { .. }
            | ProvisionError::Cleanup(_)
            | ProvisionError::CleanupFailed { .. } => RpcErrorCode::Internal,
        }
    }

    fn evidence(&self) -> Option<Value> {
        let Self::CleanupAfter { cleanup, .. } = self else {
            return None;
        };
        let mut lines = vec![cleanup.to_string()];
        lines.extend(error_chain::causes(cleanup.as_ref()));
        Some(serde_json::json!({ "cleanup": lines }))
    }

    fn hints(&self) -> Vec<Hint> {
        if let Self::CleanupAfter { remove, .. } = self {
            return vec![Hint::Next(remove.to_string())];
        }
        Vec::new()
    }
}

impl Reported for PushError {
    fn code(&self) -> RpcErrorCode {
        match self {
            PushError::VariantUnavailable { .. } => RpcErrorCode::NotFound,
            PushError::BuildIncomplete { .. } => RpcErrorCode::Conflict,
            PushError::InvalidReference { .. } | PushError::InvalidSelector(_) => {
                RpcErrorCode::InvalidArgument
            }
            PushError::Selector(error) => error.code(),
            PushError::Cluster(error) => error.code(),
            PushError::ImageIngest(error) | PushError::PeerPull(error) => error.code.clone(),
            PushError::UnsupportedImageStore => RpcErrorCode::Unsupported,
            // Ctrl-C or the caller stopped delivery: not a fault; it did not finish and can be retried.
            PushError::Cancelled => RpcErrorCode::Unavailable,
        }
    }
}

impl Reported for CodecError {
    fn code(&self) -> RpcErrorCode {
        match self {
            CodecError::UnsupportedCommand(_) | CodecError::UnsupportedProtocolMajor { .. } => {
                RpcErrorCode::Unsupported
            }
            CodecError::EncodeJson(_)
            | CodecError::DecodeJson(_)
            | CodecError::UnexpectedResponse { .. }
            | CodecError::UnexpectedRequest { .. } => RpcErrorCode::Internal,
        }
    }
}

impl Reported for cloud_enroll::Error {
    fn code(&self) -> RpcErrorCode {
        match self {
            cloud_enroll::Error::Timeout(_)
            | cloud_enroll::Error::Connect(_)
            | cloud_enroll::Error::Http(_)
            | cloud_enroll::Error::RetrySameCommand { .. }
            | cloud_enroll::Error::CloudCantReach
            | cloud_enroll::Error::FounderWait => RpcErrorCode::Unavailable,
            cloud_enroll::Error::Json(_) => RpcErrorCode::Internal,
            cloud_enroll::Error::TokenRefused => RpcErrorCode::Unauthenticated,
            cloud_enroll::Error::Status { status, .. } => http_status_code(*status),
            cloud_enroll::Error::CodedConflict { .. } => RpcErrorCode::Conflict,
        }
    }
}

impl Reported for ConfigError {
    fn code(&self) -> RpcErrorCode {
        match self {
            ConfigError::Context(error) => error.code(),
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
}

impl Reported for EnrollmentHistoryError {
    fn code(&self) -> RpcErrorCode {
        match self {
            EnrollmentHistoryError::Allocation(_) => RpcErrorCode::Conflict,
            EnrollmentHistoryError::Io(_)
            | EnrollmentHistoryError::Serialization(_)
            | EnrollmentHistoryError::MissingScope => RpcErrorCode::Internal,
        }
    }
}

impl Reported for ContextError {
    fn code(&self) -> RpcErrorCode {
        match self {
            ContextError::NoConfig
            | ContextError::NoContexts(_)
            | ContextError::ContextNotFound { .. }
            | ContextError::NoConnections { .. } => RpcErrorCode::NotFound,
            ContextError::NoCurrentContext(_) | ContextError::Connection(_) => {
                RpcErrorCode::InvalidArgument
            }
        }
    }
}

impl Reported for Message {
    fn code(&self) -> RpcErrorCode {
        self.code.clone()
    }

    fn evidence(&self) -> Option<Value> {
        Some(self.details.clone())
    }
}

impl Reported for RpcError {
    fn code(&self) -> RpcErrorCode {
        self.code.clone()
    }

    fn evidence(&self) -> Option<Value> {
        Some(self.details.clone())
    }
}

impl Reported for TransportError {
    fn code(&self) -> RpcErrorCode {
        self.to_rpc_error().code
    }

    fn evidence(&self) -> Option<Value> {
        Some(self.to_rpc_error().details)
    }
}

impl Reported for PublishError {
    fn code(&self) -> RpcErrorCode {
        PublishError::code(self)
    }
}

impl Reported for LoginError {
    fn code(&self) -> RpcErrorCode {
        login_code(self)
    }

    fn evidence(&self) -> Option<Value> {
        matches!(self, Self::Unanswered { .. }).then(|| serde_json::json!({ "writes": "unknown" }))
    }
}

impl Reported for io::Error {
    fn code(&self) -> RpcErrorCode {
        io_code(self)
    }
}

impl Reported for MachineUpdateError {
    fn code(&self) -> RpcErrorCode {
        match self {
            Self::DuplicateName => RpcErrorCode::Conflict,
            Self::MissingEndpoints => RpcErrorCode::InvalidArgument,
        }
    }
}

impl Reported for ServiceSelectorError {
    fn code(&self) -> RpcErrorCode {
        match self {
            Self::NotFound { .. } => RpcErrorCode::NotFound,
            Self::NameAmbiguity { .. } => RpcErrorCode::Ambiguous,
        }
    }
}

impl Reported for ContainerSelectorError {
    fn code(&self) -> RpcErrorCode {
        match self {
            Self::NotFound { .. } => RpcErrorCode::NotFound,
            Self::Ambiguous { .. } => RpcErrorCode::Ambiguous,
        }
    }

    fn hints(&self) -> Vec<Hint> {
        match self {
            Self::NotFound { valid, .. } => vec![Hint::valid(valid)],
            Self::Ambiguous { .. } => Vec::new(),
        }
    }
}

impl Reported for crate::cluster::RoleWaitError {
    fn code(&self) -> RpcErrorCode {
        match self {
            Self::Connect(error) => error.code(),
            Self::NotObserved(_) | Self::ChangeNotObserved | Self::Cancelled => {
                RpcErrorCode::Unavailable
            }
        }
    }
}

/// Bad input, whatever its wording.
macro_rules! invalid_argument {
    ($($t:ty),+ $(,)?) => {
        $(impl Reported for $t {
            fn code(&self) -> RpcErrorCode {
                RpcErrorCode::InvalidArgument
            }
        })+
    };
}

invalid_argument!(
    ValueError,
    ConnectionError,
    NamespaceError,
    std::num::ParseIntError,
    shell_words::ParseError,
    UnconfirmedDataLoss,
);

/// A real fault.
macro_rules! internal {
    ($($t:ty),+ $(,)?) => {
        $(impl Reported for $t {
            fn code(&self) -> RpcErrorCode {
                RpcErrorCode::Internal
            }
        })+
    };
}

internal!(StreamProtocolError, serde_json::Error);
