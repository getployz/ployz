//! The `--json` code and details of each error the CLI reports, pinned per variant.

use std::{error::Error, io};

use ployz_core::{
    CodecError, ContainerSelector, ContainerSelectorError, MachineId, MachineSelectorError,
    MachineTarget, MachineUpdateError, RpcError, RpcErrorCode, ServiceName, ServiceSelector,
    ServiceSelectorError, StreamProtocolError,
};

use super::classify;
use crate::{
    cloud_enroll,
    cloud_login::LoginError,
    connect::{ConnectError, TransportError},
    context::{ConfigError, ConnectionSource, ContextError},
    deploy::PlanError,
    enrollment::local::Error as EnrollmentHistoryError,
    image::PushError,
    operator::{OperatorError, input::InputError},
    provisioning::ProvisionError,
    published::PublishError,
};

type Row = (&'static str, Box<dyn Error + Send + Sync + 'static>);

fn row(label: &'static str, error: impl Error + Send + Sync + 'static) -> Row {
    (label, Box::new(error))
}

fn machine() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

fn reqwest_error() -> reqwest::Error {
    reqwest::Client::new().get("http://[").build().unwrap_err()
}

fn transport(code: tonic::Code) -> TransportError {
    TransportError::from(tonic::Status::new(code, "no"))
}

fn rpc(code: RpcErrorCode) -> RpcError {
    RpcError::new(code, "refused").with_details(serde_json::json!({ "next": "ployz server ls" }))
}

fn service() -> ServiceSelector {
    ServiceSelector::parse("api").unwrap()
}

fn container() -> ContainerSelector {
    ContainerSelector::parse("api-1").unwrap()
}

fn connect_rows() -> Vec<Row> {
    let all_failed = |last: Option<Box<ConnectError>>| ConnectError::AllFailed {
        selection: ConnectionSource::Direct,
        attempts: 2,
        server: None,
        relayed: false,
        retry: ployz_core::Retry::AfterRestart,
        reached: false,
        last,
    };
    vec![
        row(
            "connect/identity-mismatch",
            ConnectError::IdentityMismatch {
                expected: machine(),
                actual: machine(),
            },
        ),
        row(
            "connect/cluster-mismatch",
            ConnectError::ClusterMismatch {
                machine_id: machine(),
            },
        ),
        row(
            "connect/cluster-unverified",
            ConnectError::ClusterUnverified {
                machine_id: machine(),
            },
        ),
        row("connect/attempt", ConnectError::Attempt("no route".into())),
        row(
            "connect/exhausted",
            ConnectError::Exhausted(Box::new(io::Error::other("x"))),
        ),
        row("connect/entry-not-ready", ConnectError::EntryNotReady),
        row("connect/io", ConnectError::Io(io::Error::other("x"))),
        row(
            "connect/missing-machine-details",
            ConnectError::MissingMachineDetails,
        ),
        row("connect/client-refused", ConnectError::ClientRefused),
        row("connect/client-cleared", ConnectError::ClientCleared),
        row("connect/tunnel-needs-full", ConnectError::TunnelNeedsFull),
        row("connect/older-machine", ConnectError::OlderMachine),
        row("connect/ssh-client-missing", ConnectError::SshClientMissing),
        row(
            "connect/proxy-unsupported",
            ConnectError::ProxyUnsupported("p".into()),
        ),
        row(
            "connect/unsupported-network",
            ConnectError::UnsupportedNetwork("n".into()),
        ),
        row(
            "connect/config",
            ConnectError::Config(ConfigError::Context(ContextError::NoConfig)),
        ),
        row(
            "connect/connection",
            ConnectError::Connection(crate::context::ConnectionError::ManagementCapability),
        ),
        row(
            "connect/context",
            ConnectError::Context(ContextError::NoConfig),
        ),
        row(
            "connect/path",
            ConnectError::Path {
                path: "p".into(),
                source: io::Error::other("x"),
            },
        ),
        row(
            "connect/all-failed-last",
            all_failed(Some(Box::new(ConnectError::OlderMachine))),
        ),
        row("connect/all-failed-none", all_failed(None)),
        row(
            "connect/rpc-unavailable",
            ConnectError::Rpc(transport(tonic::Code::Unavailable)),
        ),
        row(
            "connect/rpc-permission-denied",
            ConnectError::Rpc(transport(tonic::Code::PermissionDenied)),
        ),
        row(
            "connect/codec",
            ConnectError::Codec(CodecError::UnsupportedCommand("c".into())),
        ),
        row(
            "connect/remote",
            ConnectError::Remote(rpc(RpcErrorCode::Conflict)),
        ),
        row(
            "connect/value",
            ConnectError::Value(MachineTarget::parse("").unwrap_err()),
        ),
    ]
}

fn plain_rows() -> Vec<Row> {
    let service_name = || ServiceName::parse("api").unwrap();
    let volume = || ployz_core::DockerVolumeName::parse("data").unwrap();
    let machine_name = || ployz_core::MachineName::parse("beta").unwrap();
    vec![
        row("context/no-config", ContextError::NoConfig),
        row("context/no-contexts", ContextError::NoContexts("p".into())),
        row(
            "context/no-current-context",
            ContextError::NoCurrentContext("p".into()),
        ),
        row(
            "context/not-found",
            ContextError::ContextNotFound {
                name: "n".into(),
                path: "p".into(),
            },
        ),
        row(
            "context/no-connections",
            ContextError::NoConnections { name: "n".into() },
        ),
        row(
            "context/connection",
            ContextError::Connection(crate::context::ConnectionError::ManagementCapability),
        ),
        row(
            "config/management-connection-missing",
            ConfigError::ManagementConnectionMissing,
        ),
        row(
            "config/context",
            ConfigError::Context(ContextError::NoConfig),
        ),
        row(
            "config/read",
            ConfigError::Read {
                path: "p".into(),
                source: io::Error::other("x"),
            },
        ),
        row(
            "config/private-permissions",
            ConfigError::PrivatePermissions("p".into()),
        ),
        row(
            "config/empty-current-context",
            ConfigError::EmptyCurrentContext("p".into()),
        ),
        row(
            "machine-selector/no-targets",
            MachineSelectorError::NoTargets,
        ),
        row(
            "machine-selector/no-visible",
            MachineSelectorError::NoVisibleMachines,
        ),
        row(
            "machine-selector/not-found",
            MachineSelectorError::NotFound(vec![MachineTarget::parse("m-1").unwrap()]),
        ),
        row(
            "machine-selector/ambiguous",
            MachineSelectorError::Ambiguous {
                selector: MachineTarget::parse("m-1").unwrap(),
                matches: vec![machine()],
            },
        ),
        row(
            "service-selector/not-found",
            ServiceSelectorError::NotFound {
                selector: service(),
            },
        ),
        row(
            "service-selector/ambiguous",
            ServiceSelectorError::NameAmbiguity {
                selector: service(),
                matches: Vec::new(),
            },
        ),
        row(
            "container-selector/not-found",
            ContainerSelectorError::NotFound {
                selector: container(),
                valid: vec!["api-1".into(), "api-2".into()],
            },
        ),
        row(
            "container-selector/not-found-none-valid",
            ContainerSelectorError::NotFound {
                selector: container(),
                valid: Vec::new(),
            },
        ),
        row(
            "container-selector/ambiguous",
            ContainerSelectorError::Ambiguous {
                selector: container(),
                container_ids: Vec::new(),
            },
        ),
        row(
            "machine-update/duplicate",
            MachineUpdateError::DuplicateName,
        ),
        row(
            "machine-update/missing-endpoints",
            MachineUpdateError::MissingEndpoints,
        ),
        row("stream-protocol", StreamProtocolError::TruncatedHeader),
        row(
            "codec/unsupported-command",
            CodecError::UnsupportedCommand("c".into()),
        ),
        row(
            "codec/unsupported-major",
            CodecError::UnsupportedProtocolMajor {
                requested: 2,
                supported: 1,
            },
        ),
        row(
            "codec/unexpected-response",
            CodecError::UnexpectedResponse {
                expected: "a",
                actual: "b".into(),
            },
        ),
        row("io/not-found", io::Error::from(io::ErrorKind::NotFound)),
        row(
            "io/invalid-input",
            io::Error::from(io::ErrorKind::InvalidInput),
        ),
        row(
            "io/connection-refused",
            io::Error::from(io::ErrorKind::ConnectionRefused),
        ),
        row("io/timed-out", io::Error::from(io::ErrorKind::TimedOut)),
        row("io/other", io::Error::other("x")),
        row("json", serde_json::from_str::<u8>("x").unwrap_err()),
        row("parse-int", "x".parse::<u8>().unwrap_err()),
        row("shell-words", shell_words::split("'").unwrap_err()),
        row("value", MachineTarget::parse("").unwrap_err()),
        row(
            "namespace/reserved",
            crate::namespace::NamespaceError::Reserved {
                name: ployz_core::Namespace::parse("ployz").unwrap(),
            },
        ),
        row("transport/unavailable", transport(tonic::Code::Unavailable)),
        row(
            "transport/permission-denied",
            transport(tonic::Code::PermissionDenied),
        ),
        row("transport/not-found", transport(tonic::Code::NotFound)),
        row("rpc/conflict", rpc(RpcErrorCode::Conflict)),
        row(
            "rpc/null-details",
            RpcError::new(RpcErrorCode::Internal, "x"),
        ),
        row(
            "enrollment-history/io",
            EnrollmentHistoryError::Io(io::Error::other("x")),
        ),
        row(
            "enrollment-history/missing-scope",
            EnrollmentHistoryError::MissingScope,
        ),
        row(
            "role-wait/not-observed",
            crate::cluster::RoleWaitError::NotObserved(crate::cluster::RoleSetting::ServicesOff),
        ),
        row(
            "role-wait/change-not-observed",
            crate::cluster::RoleWaitError::ChangeNotObserved,
        ),
        row(
            "role-wait/cancelled",
            crate::cluster::RoleWaitError::Cancelled,
        ),
        row(
            "role-wait/connect",
            crate::cluster::RoleWaitError::Connect(ConnectError::OlderMachine),
        ),
        row("plan/capacity-unknown", PlanError::CapacityUnknown),
        row(
            "plan/insufficient-capacity",
            PlanError::InsufficientCapacity,
        ),
        row(
            "plan/service-mode-cannot-change",
            PlanError::ServiceModeCannotChange,
        ),
        row(
            "plan/storage-unavailable",
            PlanError::ProvisionedVolumeStorageUnavailable,
        ),
        row(
            "plan/dependency-cycle",
            PlanError::DependencyCycle {
                service: "api".into(),
            },
        ),
        row(
            "plan/service-wraps-invalid",
            PlanError::Service {
                service: "api".into(),
                source: Box::new(PlanError::DependencyCycle {
                    service: "api".into(),
                }),
            },
        ),
        row(
            "plan/service-wraps-conflict",
            PlanError::Service {
                service: "api".into(),
                source: Box::new(PlanError::HostPortConflict {
                    service: service_name(),
                }),
            },
        ),
        row(
            "plan/conflicting-host-publications",
            PlanError::ConflictingHostPublications {
                service: service_name(),
            },
        ),
        row(
            "plan/duplicate-target",
            PlanError::DuplicateTargetService {
                service: service_name(),
            },
        ),
        row(
            "plan/removal-in-target",
            PlanError::RemovalInTarget {
                service: service_name(),
            },
        ),
        row(
            "plan/removal-not-selected",
            PlanError::RemovalNotSelected {
                service: service_name(),
            },
        ),
        row(
            "plan/mixed-volume-modes",
            PlanError::MixedVolumeModes {
                name: volume(),
                global: "g".into(),
                replicated: "r".into(),
            },
        ),
        row(
            "plan/conflicting-docker-volumes",
            PlanError::ConflictingDockerVolumeDefinitions { name: volume() },
        ),
        row(
            "plan/copies-unknown",
            PlanError::CopiesUnknown {
                name: "v".into(),
                machine: machine_name(),
            },
        ),
        row(
            "plan/holder-unknown",
            PlanError::VolumeHolderUnknown {
                name: "v".into(),
                machine: machine_name(),
            },
        ),
        row(
            "plan/storage-unknown",
            PlanError::ProvisionedVolumeStorageUnknown {
                names: vec![machine_name()],
            },
        ),
        row(
            "plan/storage-required",
            PlanError::ProvisionedVolumeStorageRequired {
                machine: machine_name(),
            },
        ),
        row(
            "plan/server-takes-no-services",
            PlanError::VolumeServerTakesNoServices {
                volume: "v".into(),
                machine: machine_name(),
            },
        ),
        row(
            "plan/existing-plain-volume",
            PlanError::ExistingPlainVolume {
                name: volume(),
                machine: machine_name(),
            },
        ),
    ]
}

fn operator_rows() -> Vec<Row> {
    let unanswered = || ployz_core::PartialResult::<(), RpcError> {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    vec![
        row(
            "operator/connect",
            OperatorError::Connect(ConnectError::OlderMachine),
        ),
        row(
            "operator/selector",
            OperatorError::Selector(ServiceSelectorError::NotFound {
                selector: service(),
            }),
        ),
        row(
            "operator/not-running",
            OperatorError::NotRunning {
                name: "api".into(),
                running_in_scope: vec!["a".into(), "b".into()],
            },
        ),
        row(
            "operator/not-running-on-answered",
            OperatorError::NotRunningOnAnswered {
                service: "api".into(),
                gaps: unanswered(),
                names: crate::operator::MachineNames::default(),
            },
        ),
        row(
            "operator/machine-selector",
            OperatorError::MachineSelector(MachineSelectorError::NoTargets),
        ),
        row(
            "operator/value",
            OperatorError::Value(MachineTarget::parse("").unwrap_err()),
        ),
        row(
            "operator/container-not-found",
            OperatorError::Container(ContainerSelectorError::NotFound {
                selector: container(),
                valid: vec!["api-1".into()],
            }),
        ),
        row(
            "operator/container-ambiguous",
            OperatorError::Container(ContainerSelectorError::Ambiguous {
                selector: container(),
                container_ids: Vec::new(),
            }),
        ),
        row(
            "operator/no-regular-container",
            OperatorError::NoRegularContainer,
        ),
        row(
            "operator/rpc",
            OperatorError::Rpc(transport(tonic::Code::Unavailable)),
        ),
        row(
            "operator/protocol",
            OperatorError::Protocol(StreamProtocolError::TruncatedHeader),
        ),
        row(
            "operator/codec",
            OperatorError::Codec(CodecError::UnsupportedCommand("c".into())),
        ),
        row("operator/stream-closed", OperatorError::StreamClosed),
        row("operator/tty-requires-stdin", InputError::TtyRequiresStdin),
        row(
            "operator/invalid-service-selector",
            InputError::InvalidServiceSelector("s".into()),
        ),
        row("operator/invalid-tail", InputError::InvalidTail("t".into())),
        row(
            "operator/invalid-time",
            InputError::InvalidTime {
                flag: "since",
                value: "v".into(),
            },
        ),
        row("operator/invalid-proxy-port", InputError::InvalidProxyPort),
        row(
            "operator/invalid-local-port",
            InputError::InvalidLocalPort("p".into()),
        ),
        row(
            "operator/invalid-remote-port",
            InputError::InvalidRemotePort("p".into()),
        ),
        row(
            "operator/input",
            OperatorError::Input(InputError::InvalidTail("t".into())),
        ),
        row(
            "operator/no-servable-container",
            OperatorError::NoServableContainer,
        ),
        row("operator/no-machines", OperatorError::NoMachines),
        row("operator/no-services", OperatorError::NoServices),
        row(
            "operator/no-deployment-containers",
            OperatorError::NoDeploymentContainers,
        ),
        row("operator/snapshot-stale", OperatorError::SnapshotStale),
        row(
            "operator/unsupported-log-service",
            OperatorError::UnsupportedLogService {
                service: "s".into(),
                expected: "e",
            },
        ),
        row(
            "operator/open-container-logs",
            OperatorError::OpenContainerLogs {
                container: "c".into(),
                server: "s".into(),
                source: Box::new(OperatorError::StreamClosed),
            },
        ),
        row(
            "operator/exec-container-stopped",
            OperatorError::ExecContainerStopped {
                service: "s".into(),
            },
        ),
        row(
            "operator/start-exec-refused",
            OperatorError::StartExec {
                container: "c".into(),
                source: TransportError::from(ployz_core::rpc::hinted_status(
                    tonic::Code::PermissionDenied,
                    &io::Error::other("needs Full access"),
                    serde_json::json!({
                        "capability": "ployz.container.exec.v1",
                        "next": "ployz device request",
                    }),
                )),
            },
        ),
        row(
            "operator/exec-connection-lost",
            OperatorError::ExecConnectionLost {
                server: "s".into(),
                source: transport(tonic::Code::Unavailable),
            },
        ),
    ]
}

fn login_rows() -> Vec<Row> {
    vec![
        row(
            "login/unreachable",
            LoginError::Unreachable {
                cloud: "c".into(),
                error: reqwest_error(),
            },
        ),
        row(
            "login/unanswered",
            LoginError::Unanswered {
                cloud: "c".into(),
                error: reqwest_error(),
            },
        ),
        row("login/unsupported", LoginError::Unsupported("u".into())),
        row(
            "login/status-401",
            LoginError::Status {
                status: 401,
                body: String::new(),
            },
        ),
        row(
            "login/status-404",
            LoginError::Status {
                status: 404,
                body: String::new(),
            },
        ),
        row(
            "login/status-409",
            LoginError::Status {
                status: 409,
                body: String::new(),
            },
        ),
        row(
            "login/status-429",
            LoginError::Status {
                status: 429,
                body: String::new(),
            },
        ),
        row(
            "login/status-422",
            LoginError::Status {
                status: 422,
                body: String::new(),
            },
        ),
        row(
            "login/status-502",
            LoginError::Status {
                status: 502,
                body: String::new(),
            },
        ),
        row("login/reply", LoginError::Reply("r".into())),
        row(
            "login/store",
            LoginError::Store {
                path: "p".into(),
                source: io::Error::other("x"),
            },
        ),
        row(
            "login/corrupt",
            LoginError::Corrupt {
                path: "p".into(),
                source: serde_json::from_str::<u8>("x").unwrap_err(),
            },
        ),
        row(
            "login/token-cloud-unreadable",
            LoginError::TokenCloudUnreadable("p".into()),
        ),
        row(
            "login/other-cloud",
            LoginError::OtherCloud {
                signed_in: "a".into(),
                requested: "b".into(),
            },
        ),
        row("login/signed-out", LoginError::SignedOut),
        row("login/expired", LoginError::Expired),
        row("login/denied", LoginError::Denied),
        row("login/ended", LoginError::Ended),
        row(
            "login/awaiting-approval",
            LoginError::AwaitingApproval { url: "u".into() },
        ),
        row("login/token-refused", LoginError::TokenRefused),
        row("login/agent-token-refused", LoginError::AgentTokenRefused),
        row("login/token-malformed", LoginError::TokenMalformed),
        row("login/not-member", LoginError::NotMember("o".into())),
        row("login/token-bound", LoginError::TokenBound),
        row(
            "login/unknown-organization",
            LoginError::UnknownOrganization("o".into()),
        ),
        row(
            "login/unknown-credential",
            LoginError::UnknownCredential("c".into()),
        ),
        row(
            "login/ambiguous-credential",
            LoginError::AmbiguousCredential {
                name: "n".into(),
                ids: Vec::new(),
            },
        ),
        row(
            "enroll/json",
            cloud_enroll::Error::Json(serde_json::from_str::<u8>("x").unwrap_err()),
        ),
        row(
            "enroll/status-403",
            cloud_enroll::Error::Status {
                status: 403,
                body: String::new(),
            },
        ),
        row(
            "enroll/status-500",
            cloud_enroll::Error::Status {
                status: 500,
                body: String::new(),
            },
        ),
        row(
            "enroll/coded-conflict",
            cloud_enroll::Error::CodedConflict {
                body: String::new(),
            },
        ),
        row("enroll/token-refused", cloud_enroll::Error::TokenRefused),
        row("enroll/founder-wait", cloud_enroll::Error::FounderWait),
        row(
            "enroll/cloud-cant-reach",
            cloud_enroll::Error::CloudCantReach,
        ),
        row(
            "enroll/timeout",
            cloud_enroll::Error::Timeout(reqwest_error()),
        ),
        row(
            "enroll/connect",
            cloud_enroll::Error::Connect(reqwest_error()),
        ),
        row("enroll/http", cloud_enroll::Error::Http(reqwest_error())),
    ]
}

fn provision_and_publish_rows() -> Vec<Row> {
    let provision_io = || io::Error::other("x");
    vec![
        row(
            "provision/missing-destination",
            ProvisionError::MissingDestination,
        ),
        row(
            "provision/remote-transport",
            ProvisionError::RemoteTransport("t".into()),
        ),
        row(
            "provision/connection",
            ProvisionError::Connection(crate::context::ConnectionError::ManagementCapability),
        ),
        row(
            "provision/ssh-client-missing",
            ProvisionError::SshClientMissing,
        ),
        row("provision/whoami", ProvisionError::Whoami(provision_io())),
        row(
            "provision/whoami-failed",
            ProvisionError::WhoamiFailed("w".into()),
        ),
        row("provision/whoami-utf8", ProvisionError::WhoamiUtf8),
        row("provision/empty-user", ProvisionError::EmptyUser),
        row("provision/sudo", ProvisionError::Sudo(provision_io())),
        row(
            "provision/sudo-required",
            ProvisionError::SudoRequired { user: "u".into() },
        ),
        row(
            "provision/platform",
            ProvisionError::Platform(provision_io()),
        ),
        row(
            "provision/platform-failed",
            ProvisionError::PlatformFailed("p".into()),
        ),
        row("provision/platform-utf8", ProvisionError::PlatformUtf8),
        row("provision/unsupported-os", ProvisionError::UnsupportedOs),
        row(
            "provision/unsupported-architecture",
            ProvisionError::UnsupportedArchitecture("a".into()),
        ),
        row(
            "provision/bootstrap-io",
            ProvisionError::BootstrapIo {
                stage: "s",
                source: provision_io(),
            },
        ),
        row(
            "provision/bootstrap-verification",
            ProvisionError::BootstrapVerification("v".into()),
        ),
        row(
            "provision/bootstrap-unsigned",
            ProvisionError::BootstrapUnsigned(ployz_core::MachineVersion::parse("0.3.0").unwrap()),
        ),
        row(
            "provision/transfer",
            ProvisionError::Transfer(provision_io()),
        ),
        row("provision/install", ProvisionError::Install(provision_io())),
        row(
            "provision/install-refused",
            ProvisionError::InstallRefused { reason: "r".into() },
        ),
        row("provision/cleanup", ProvisionError::Cleanup(provision_io())),
        row("provision/not-root", ProvisionError::NotRoot),
        row(
            "provision/zfs-without-installer",
            ProvisionError::ZfsWithoutInstaller,
        ),
        row(
            "provision/cleanup-after",
            ProvisionError::CleanupAfter {
                primary: Box::new(ProvisionError::UnsupportedOs),
                causes: None,
                cleanup: Box::new(ProvisionError::Cleanup(provision_io())),
                remove: "ployz server rm beta".into(),
            },
        ),
        row(
            "push/variant-unavailable",
            PushError::VariantUnavailable {
                image: "i".into(),
                machine_id: machine(),
                platform: "p".into(),
            },
        ),
        row(
            "push/build-incomplete",
            PushError::BuildIncomplete {
                image: "i".into(),
                machine_id: machine(),
                missing: Vec::new(),
            },
        ),
        row(
            "push/invalid-reference",
            PushError::InvalidReference {
                reference: "r".into(),
                message: "m".into(),
            },
        ),
        row("push/cancelled", PushError::Cancelled),
        row(
            "push/invalid-selector",
            PushError::InvalidSelector(MachineTarget::parse("").unwrap_err()),
        ),
        row(
            "push/selector",
            PushError::Selector(MachineSelectorError::NoTargets),
        ),
        row(
            "push/cluster",
            PushError::Cluster(ConnectError::OlderMachine),
        ),
        row(
            "push/image-ingest",
            PushError::ImageIngest(rpc(RpcErrorCode::Conflict)),
        ),
        row(
            "push/peer-pull",
            PushError::PeerPull(rpc(RpcErrorCode::Unavailable)),
        ),
        row(
            "push/unsupported-image-store",
            PushError::UnsupportedImageStore,
        ),
        row(
            "publish/unsupported",
            PublishError::Unsupported { cloud: "c".into() },
        ),
        row("publish/unavailable", PublishError::Unavailable("u".into())),
        row(
            "publish/refused",
            PublishError::Refused(rpc(RpcErrorCode::Conflict)),
        ),
        row("publish/cloud", PublishError::Cloud(LoginError::SignedOut)),
        row(
            "publish/malformed",
            PublishError::Malformed {
                cloud: "c".into(),
                reason: "r".into(),
            },
        ),
        row(
            "publish/runtime",
            PublishError::Runtime(io::Error::other("x")),
        ),
        row("publish/gone", PublishError::Gone { path: "p".into() }),
        row(
            "publish/needs-terminal",
            PublishError::NeedsTerminal { path: "p".into() },
        ),
        row(
            "publish/agent-refused",
            PublishError::AgentRefused { path: "p".into() },
        ),
        row(
            "publish/other-cloud",
            PublishError::OtherCloud {
                published: "a".into(),
                acting: "b".into(),
            },
        ),
        row(
            "publish/slow",
            PublishError::Slow {
                cloud: "c".into(),
                budget: std::time::Duration::from_secs(2),
            },
        ),
    ]
}

fn table() -> String {
    connect_rows()
        .into_iter()
        .chain(plain_rows())
        .chain(operator_rows())
        .chain(login_rows())
        .chain(provision_and_publish_rows())
        .map(|(label, error)| {
            let (code, details) = classify(error.as_ref());
            format!("{label} | {} | {details}\n", code.as_str())
        })
        .collect()
}

#[test]
fn every_reported_error_keeps_its_code_and_details() {
    let table = table();
    let golden = include_str!("code_table.golden");
    assert!(
        table == golden,
        "the table changed; actual:\n{table}\n---\nexpected:\n{golden}"
    );
}
