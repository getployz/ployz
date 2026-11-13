mod reported;

use ployz_core::error_chain;
use std::{borrow::Cow, error::Error, fmt, io};

use ployz_core::{CapabilityRefusal, Hint, RpcError, RpcErrorCode};
use serde_json::Value;

use reported::Reported as _;

use crate::{
    cloud_login::LoginError,
    connect::{ConnectError, TransportError},
    context::ContextError,
    deploy::DeployError,
    image::PushError,
    operator::OperatorError,
    ui,
};

/// CLI command outcome. `Display` is our sentence; `ui::exit` prints it.
#[derive(Debug)]
pub struct Failure {
    inner: Inner,
    interrupted: bool,
    /// The printed result is itself the failure, not a partial one.
    outcome: bool,
    diagnostic: Option<Box<ui::progress::Diagnostic>>,
    /// Data its `--json` error carries in `details` beside what the error says.
    added: serde_json::Map<String, Value>,
    connect_failure: Option<Box<ConnectFailure>>,
}

/// Why no Server of a Cluster could carry a command: a fact the route found out, kept as a
/// value so no caller recognises it by the text or the `--json` keys it was reported with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConnectFailure {
    /// Ployz Cloud knows no Server of this Organization.
    NoServers,
    /// Servers Cloud knows, none of which answered, named where Cloud holds a name.
    Unanswered { servers: Vec<String>, relayed: bool },
    /// A Server refused to give this command access.
    Refused,
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
    fn of(inner: Inner) -> Self {
        Self {
            inner,
            interrupted: false,
            outcome: false,
            diagnostic: None,
            added: serde_json::Map::new(),
            connect_failure: None,
        }
    }

    pub(crate) fn command(error: impl Error + Send + Sync + 'static) -> Self {
        Self::of(Inner::Command {
            error: Box::new(error),
            hints: Vec::new(),
        })
    }

    /// Add a line telling the reader what to do about this failure.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<Option<Hint>>) -> Self {
        if let (Inner::Command { hints, .. }, Some(hint)) = (&mut self.inner, hint.into()) {
            hints.push(hint);
        }
        self
    }

    pub(crate) fn context_error(&self) -> Option<&ContextError> {
        std::iter::successors(self.source(), |&link| link.source())
            .find_map(|link| link.downcast_ref::<ContextError>())
    }

    /// End the process with `code`, whatever it printed already said: the remote
    /// command's exit code, a cancelled prompt, or clap's help.
    #[must_use]
    pub fn exit(code: u8) -> Self {
        Self::of(Inner::Exit(code))
    }

    /// The result is printed, but some targets failed or never answered.
    #[must_use]
    pub fn partial() -> Self {
        Self::of(Inner::Partial)
    }

    /// The invocation was interrupted; cancellation takes precedence over a committed result.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::coded(RpcErrorCode::Internal, "Cancelled.").interrupted()
    }

    pub(crate) fn interrupted(mut self) -> Self {
        self.interrupted = true;
        self
    }

    pub(crate) const fn is_interrupted(&self) -> bool {
        self.interrupted
    }

    /// The result already printed is this failure, such as a Deployment that failed:
    /// it exits by its code rather than as a partial result.
    pub(crate) fn outcome(mut self) -> Self {
        self.outcome = true;
        self
    }

    pub(crate) const fn is_outcome(&self) -> bool {
        self.outcome
    }

    pub(crate) fn with_diagnostic(mut self, diagnostic: ui::progress::Diagnostic) -> Self {
        self.diagnostic = Some(Box::new(diagnostic));
        self
    }

    /// Carry `value` as `key` in the `--json` error's `details`.
    #[must_use]
    pub(crate) fn with_detail(mut self, key: &str, value: Value) -> Self {
        self.added.insert(key.to_owned(), value);
        self
    }

    /// A Server's daemon answered before this connection failed: a refusal, not an
    /// unreachable Server.
    pub(crate) fn daemon_answered(&self) -> bool {
        self.connect_error().is_some_and(|error| !error.unreached())
    }

    /// The connection failure this failure carries, if any.
    pub(crate) fn connect_error(&self) -> Option<&ConnectError> {
        let Inner::Command { error, .. } = &self.inner else {
            return None;
        };
        let mut next: Option<&(dyn Error + 'static)> = Some(error.as_ref());
        while let Some(error) = next {
            if let Some(error) = error.downcast_ref::<ConnectError>() {
                return Some(error);
            }
            next = error.source();
        }
        None
    }

    pub(crate) fn with_connect_failure(mut self, failure: ConnectFailure) -> Self {
        self.connect_failure = Some(Box::new(failure));
        self
    }

    pub(crate) fn connect_failure(&self) -> Option<&ConnectFailure> {
        self.connect_failure.as_deref()
    }

    pub(crate) fn diagnostic(&self) -> Option<&ui::progress::Diagnostic> {
        self.diagnostic.as_deref()
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

    /// [`Self::detailed`] over `cause`, when there is one, which prints as its `cause:` line.
    pub(crate) fn detailed_caused(
        code: RpcErrorCode,
        message: impl Into<Cow<'static, str>>,
        details: Value,
        cause: Option<impl Error + Send + Sync + 'static>,
    ) -> Self {
        Self::command(Message {
            code,
            text: message.into(),
            details,
            source: cause.map(|cause| Box::new(cause) as Box<dyn Error + Send + Sync>),
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
    pub(crate) fn context(mut self, message: impl Into<Cow<'static, str>>) -> Self {
        let RpcError { code, details, .. } = self.report();
        let interrupted = self.interrupted;
        let outcome = self.outcome;
        let diagnostic = self.diagnostic.take();
        let connect_failure = self.connect_failure.take();
        let mut contextual = Self::command(Message {
            code,
            text: message.into(),
            details,
            source: Some(Box::new(self)),
        });
        contextual.interrupted = interrupted;
        contextual.outcome = outcome;
        contextual.diagnostic = diagnostic;
        contextual.connect_failure = connect_failure;
        contextual
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

    /// The `cause:` lines under our sentence. A refusal's sentence stands alone.
    #[must_use]
    pub fn causes(&self) -> Vec<String> {
        match &self.inner {
            Inner::Command { error, .. } if refusal_before_context(error.as_ref()).is_none() => {
                error_chain::causes(error.as_ref())
            }
            Inner::Command { .. } | Inner::Partial | Inner::Exit(_) => Vec::new(),
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
            Inner::Command { error, .. } => match refusal_before_context(error.as_ref()) {
                Some(refusal) => (RpcErrorCode::Unsupported, refusal.details),
                None => classify(error.as_ref()),
            },
            Inner::Partial | Inner::Exit(_) => (RpcErrorCode::Internal, Value::Null),
        };
        Hint::into_details(self.own_hints(), &mut details);
        if let Some(diagnostic) = &self.diagnostic {
            if !details.is_object() {
                details = serde_json::json!({});
            }
            details
                .as_object_mut()
                .expect("error details object")
                .insert("progress".into(), diagnostic.json());
        }
        if !self.added.is_empty() {
            if !details.is_object() {
                details = serde_json::json!({});
            }
            details
                .as_object_mut()
                .expect("error details object")
                .extend(self.added.clone());
        }
        RpcError::new(code, self.to_string())
            .with_details(details)
            .with_causes(self.causes())
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

/// The `--json` code and details of `error`; an error no one listed in `reported` is a fault.
fn classify(error: &(dyn Error + 'static)) -> (RpcErrorCode, Value) {
    reported::reported(error).unwrap_or((RpcErrorCode::Internal, Value::Null))
}

fn refusal_before_context(error: &(dyn Error + 'static)) -> Option<CapabilityRefusal> {
    std::iter::successors(Some(error), |&link| link.source())
        .take_while(|link| !link.is::<Message>())
        .find_map(|link| {
            let (code, details) = classify(link);
            CapabilityRefusal::new(&code, details)
        })
}

/// A failed image copy as the SDK reports it.
pub(crate) fn push_rpc_error(error: PushError) -> RpcError {
    RpcError::new(error.code(), error.to_string())
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

pub(crate) fn io_code(error: &io::Error) -> RpcErrorCode {
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

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            Inner::Command { error, .. } => match refusal_before_context(error.as_ref()) {
                Some(refusal) => refusal.fmt(f),
                None => error.fmt(f),
            },
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

impl From<ConnectError> for Failure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "opaque Failure peels Display-changing wrappers; the rest keep the original error"
    )]
    fn from(error: ConnectError) -> Self {
        match error {
            ConnectError::Context(error) => error.into(),
            ConnectError::Value(error) => error.into(),
            // The daemon answered with its own failure: print that, not a failed call.
            ConnectError::Rpc(error) if error.answered() => {
                Self::command(ConnectError::Remote(error.to_rpc_error()))
            }
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
            OperatorError::Rpc(error) => ConnectError::Rpc(error).into(),
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
    /// Sign-in failures name the command that fixes them. A refusal Cloud sent in the
    /// RPC error vocabulary, such as `claim_required`, keeps its code, details and next step;
    /// a route's `PublicError` keeps its sentence.
    fn from(error: LoginError) -> Self {
        if let LoginError::Status { body, .. } = &error
            && let Some(refused) = crate::cloud_account::refusal(body.as_bytes())
                .or_else(|| crate::cloud_account::public_error(body.as_bytes()))
        {
            return Self::command(refused);
        }
        if let LoginError::Store { path, source } = &error
            && source.kind() == io::ErrorKind::PermissionDenied
        {
            let next = format!("chmod 600 {}", shell_words::quote(&path.to_string_lossy()));
            return Self::command(error).hint(Hint::Next(next));
        }
        let next = match &error {
            // Cloud binds every call to the acting Organization, `org ls` included: a
            // sign-in left outside it can only start over.
            LoginError::Corrupt { .. }
            | LoginError::TokenCloudUnreadable(_)
            | LoginError::OtherCloud { .. }
            | LoginError::NotMember(_) => Some("ployz logout"),
            LoginError::SignedOut
            | LoginError::Expired
            | LoginError::Denied
            | LoginError::Ended => Some("ployz login"),
            LoginError::AwaitingApproval { .. } => Some("ployz login --wait"),
            LoginError::TokenRefused => Some(crate::cloud_account::SIGN_IN_PAST_TOKEN),
            LoginError::AgentTokenRefused => Some("ployz mcp login"),
            LoginError::UnknownOrganization(_) => Some("ployz org ls"),
            LoginError::UnknownCredential(_) | LoginError::AmbiguousCredential { .. } => {
                Some("ployz token ls")
            }
            LoginError::Unreachable { .. }
            | LoginError::Unanswered { .. }
            | LoginError::Unsupported(_)
            | LoginError::Status { .. }
            | LoginError::Reply(_)
            | LoginError::Store { .. }
            | LoginError::TokenBound
            | LoginError::TokenMalformed => None,
        };
        Self::command(error).hint(next.map(|next| Hint::Next(next.into())))
    }
}

pub(crate) fn login_code(error: &LoginError) -> RpcErrorCode {
    match error {
        LoginError::Unreachable { .. } | LoginError::Unanswered { .. } => RpcErrorCode::Unavailable,
        LoginError::Unsupported(_) => RpcErrorCode::Unsupported,
        LoginError::TokenBound => RpcErrorCode::InvalidArgument,
        LoginError::Status { status, .. } => http_status_code(*status),
        LoginError::Reply(_)
        | LoginError::Store { .. }
        | LoginError::Corrupt { .. }
        | LoginError::TokenCloudUnreadable(_) => RpcErrorCode::Internal,
        LoginError::OtherCloud { .. } => RpcErrorCode::Conflict,
        LoginError::SignedOut
        | LoginError::Expired
        | LoginError::Denied
        | LoginError::Ended
        | LoginError::AwaitingApproval { .. }
        | LoginError::TokenRefused
        | LoginError::AgentTokenRefused
        | LoginError::TokenMalformed
        | LoginError::NotMember(_) => RpcErrorCode::Unauthenticated,
        LoginError::UnknownOrganization(_) | LoginError::UnknownCredential(_) => {
            RpcErrorCode::NotFound
        }
        LoginError::AmbiguousCredential { .. } => RpcErrorCode::Ambiguous,
    }
}

impl From<tonic::Status> for Failure {
    fn from(status: tonic::Status) -> Self {
        TransportError::from(status).into()
    }
}

#[cfg(test)]
mod code_table_tests;

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use ployz_core::{MachineName, MachineTarget, RpcError, RpcErrorCode, op};
    use serde_json::Value;

    use super::*;
    use crate::{
        cloud_enroll, connect::TransportError, context::ConfigError, provisioning::ProvisionError,
    };
    use ployz_core::{ContainerSelectorError, MachineSelectorError, ValueError};

    fn unsupported(details: Value) -> RpcError {
        RpcError::unsupported("unimplemented").with_details(details)
    }

    #[test]
    fn a_refusal_with_a_capability_names_the_target_and_the_capability() {
        let details =
            serde_json::json!({ "capability": "ployz.volume.mirror.v1", "target": "beta" });
        let failure = Failure::from(unsupported(details.clone()));
        assert_eq!(
            failure.to_string(),
            "Server beta needs a newer Ployz. It cannot serve ployz.volume.mirror.v1."
        );
        let report = failure.report();
        assert_eq!(report.code, RpcErrorCode::Unsupported);
        assert_eq!(report.details, details);
        assert_eq!(crate::ui::exit_code(&report.code), 1);
    }

    #[test]
    fn an_exec_refused_for_scope_points_at_a_device_request() {
        #[derive(Debug, thiserror::Error)]
        #[error("ployz.container.exec.v1 needs Full access")]
        struct NeedsFullAccess;
        let status = ployz_core::rpc::hinted_status(
            tonic::Code::PermissionDenied,
            &NeedsFullAccess,
            serde_json::json!({
                "capability": "ployz.container.exec.v1",
                "next": "ployz device request",
            }),
        );
        let failure = Failure::from(OperatorError::StartExec {
            container: "api-1".into(),
            source: TransportError::from(status).addressed_as("beta"),
        });
        assert_eq!(failure.hints(), [Hint::Next("ployz device request".into())]);
    }

    #[test]
    fn a_refusal_without_a_capability_keeps_its_sentence() {
        let failure = Failure::from(unsupported(Value::Null));
        assert_eq!(failure.to_string(), "unimplemented");
    }

    #[test]
    fn a_refusal_inside_an_operator_wrapper_still_names_the_capability() {
        let error = OperatorError::OpenContainerLogs {
            container: "api".into(),
            server: "beta".into(),
            source: Box::new(OperatorError::Rpc(
                TransportError::from(tonic::Status::unimplemented("unimplemented"))
                    .name_refusal(op::TailLogs::PATH, &MachineTarget::parse("m-1").unwrap())
                    .addressed_as("beta"),
            )),
        };
        let failure = Failure::from(error);
        assert_eq!(
            failure.to_string(),
            "Server beta needs a newer Ployz. It cannot serve ployz.logs.tail.v1."
        );
        assert_eq!(failure.causes(), Vec::<String>::new());
        assert_eq!(failure.report().cause, Vec::<String>::new());
    }

    #[test]
    fn context_keeps_its_own_sentence_over_a_refusal() {
        let inner = Failure::from(unsupported(
            serde_json::json!({ "capability": "ployz.logs.tail.v1", "target": "beta" }),
        ));
        assert_eq!(
            inner.context("Could not deploy.").to_string(),
            "Could not deploy."
        );
    }

    #[test]
    fn context_keeps_the_inner_details_and_next() {
        let inner = Failure::from(
            RpcError::unavailable("Machine is starting")
                .with_details(serde_json::json!({ "next": "ployz server ls", "machine": "alpha" })),
        );
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
            error_chain::row(&failure),
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
                    line: String::new(),
                    source: serde_norway::from_str::<String>("[").unwrap_err(),
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
            relayed: false,
            selection: crate::context::ConnectionSource::Context("prod".into()),
            attempts: 3,
            server: None,
            retry: ployz_core::Retry::AfterRestart,
            reached: false,
            last: Some(Box::new(ConnectError::Io(io::Error::from(
                io::ErrorKind::ConnectionRefused,
            )))),
        });
        let display = failure.to_string();
        assert_eq!(
            display,
            "Ployz couldn't reach a Server's daemon through any of the 3 connections from context prod."
        );
        assert!(
            !display.contains("Os {") && !display.contains("code: 111"),
            "{display}"
        );
    }

    #[test]
    fn exhausted_connections_name_the_server_tried_last() {
        let failure = Failure::from(ConnectError::AllFailed {
            relayed: false,
            selection: crate::context::ConnectionSource::Cloud,
            attempts: 3,
            server: Some("box-node-09c8".into()),
            retry: ployz_core::Retry::AfterRestart,
            reached: false,
            last: None,
        });
        assert_eq!(
            failure.to_string(),
            "Ployz couldn't reach a Server's daemon through any of the 3 connections from Cloud. \
             Server box-node-09c8, tried last, isn't answering. Check that its Ployz daemon is running, then retry."
        );
    }

    #[test]
    fn exhausted_connections_say_a_daemon_answered_when_one_did() {
        let failure = Failure::from(ConnectError::AllFailed {
            relayed: false,
            selection: crate::context::ConnectionSource::Context("prod".into()),
            attempts: 2,
            server: None,
            retry: ployz_core::Retry::Never,
            reached: true,
            last: Some(Box::new(ConnectError::Io(io::Error::from(
                io::ErrorKind::ConnectionRefused,
            )))),
        });
        assert_eq!(
            failure.to_string(),
            "A Server's daemon answered, but the 2 connections from context prod all failed."
        );
    }

    #[test]
    fn exhausted_connections_say_the_server_tried_last_did_not_answer_after_one_refused() {
        let failure = Failure::from(ConnectError::AllFailed {
            relayed: false,
            selection: crate::context::ConnectionSource::Cloud,
            attempts: 2,
            server: Some("web-2".into()),
            retry: ployz_core::Retry::AfterRestart,
            reached: true,
            last: Some(Box::new(ConnectError::Io(io::Error::from(
                io::ErrorKind::ConnectionRefused,
            )))),
        });
        assert_eq!(
            failure.to_string(),
            "A Server's daemon answered, but the 2 connections from Cloud all failed. \
             Server web-2, tried last, isn't answering. Check that its Ployz daemon is running, then retry."
        );
    }

    #[test]
    fn a_daemon_that_answered_with_an_error_is_not_blamed_for_the_call() {
        let sent = RpcError::internal("Corrosion HTTP request failed")
            .with_causes(vec!["Connection refused (os error 111)".into()]);
        let failure = Failure::from(ConnectError::Rpc(crate::connect::TransportError::from(
            ployz_core::rpc::caused_status(tonic::Code::Internal, &sent),
        )));
        assert_eq!(failure.to_string(), "Corrosion HTTP request failed");
        assert_eq!(failure.causes(), ["Connection refused (os error 111)"]);
        assert_eq!(failure.report().code, RpcErrorCode::Internal);
        assert!(failure.daemon_answered());

        let wrapped = failure.context("Couldn't list servers.");
        assert_eq!(
            wrapped.causes().last().map(String::as_str),
            Some("Connection refused (os error 111)")
        );
        assert!(
            error_chain::row(&wrapped)
                .ends_with("Corrosion HTTP request failed: Connection refused (os error 111)"),
            "{}",
            error_chain::row(&wrapped)
        );
    }

    #[test]
    fn a_status_the_daemon_did_not_attach_details_to_keeps_the_call_wording() {
        let failure = Failure::from(ConnectError::Rpc(crate::connect::TransportError::from(
            tonic::Status::unavailable(
                "grpc-status header missing, mapped from HTTP status code 502",
            ),
        )));
        assert_eq!(
            failure.to_string(),
            "Ployz's call to your Server's daemon failed."
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
        let failure = Failure::from(RpcError::internal("boom"));
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
    fn cloud_coded_conflict_keeps_its_code() {
        let failure = Failure::from(cloud_enroll::Error::CodedConflict {
            body: serde_json::json!({"error": "Bring existing Servers online."}).to_string(),
        });
        assert_eq!(failure.report().code, RpcErrorCode::Conflict);
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
        let failure = Failure::from(crate::operator::input::InputError::InvalidTail(
            "bad".into(),
        ));
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
                    container: "web-1".into(),
                    server: "machine-1".into(),
                    source: Box::new(OperatorError::Rpc(transport())),
                }),
                Failure::from(OperatorError::StartExec {
                    container: "web-1".into(),
                    source: transport(),
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
            name: "nope".into(),
            running_in_scope: vec!["web".into()],
        });
        assert_eq!(failure.report().code, RpcErrorCode::NotFound);
        assert_eq!(failure.report().message, "No running Service \"nope\"");
        assert_eq!(failure.hints(), [Hint::valid(["web"])]);
    }

    #[test]
    fn a_missing_container_lists_the_service_containers() {
        let missing = ContainerSelectorError::NotFound {
            selector: ployz_core::ContainerSelector::parse("bogus").unwrap(),
            valid: vec!["web-1".into(), "web-2".into()],
        };
        for failure in [
            Failure::from(missing.clone()),
            Failure::from(OperatorError::Container(missing)),
        ] {
            assert_eq!(failure.report().code, RpcErrorCode::NotFound);
            assert_eq!(
                failure.report().message,
                "Container \"bogus\" was not found"
            );
            assert_eq!(failure.hints(), [Hint::valid(["web-1", "web-2"])]);
        }
    }

    #[test]
    fn a_missing_server_points_at_server_ls_locally_and_over_the_wire() {
        let missing =
            MachineSelectorError::NotFound(vec![ployz_core::MachineTarget::parse("nope").unwrap()]);
        let local = Failure::from(missing.clone());
        let wire = Failure::from(ployz_core::rpc::hinted_status(
            tonic::Code::NotFound,
            &missing,
            serde_json::json!({ "inspect": ["ployz server ls"] }),
        ));
        for failure in [local, wire] {
            assert_eq!(failure.report().code, RpcErrorCode::NotFound);
            assert_eq!(failure.to_string(), "No Server named \"nope\"");
            assert_eq!(failure.hints(), [Hint::Inspect("ployz server ls".into())]);
            assert!(failure.causes().is_empty(), "{:?}", failure.causes());
        }
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

    #[test]
    fn a_cloud_claim_required_refusal_keeps_its_code_claim_url_and_next() {
        let claim_url = "https://ployz.dev/claim/abc";
        let next = "Open https://ployz.dev/claim/abc and sign in with GitHub, then try again.";
        let body = serde_json::json!({ "error": {
            "code": "claim_required",
            "message": "Claim this Organization to create a token.",
            "details": { "claim_url": claim_url, "action": "mint_token", "next": next },
        }});
        let failure = Failure::from(LoginError::Status {
            status: 403,
            body: body.to_string(),
        });
        let report = failure.report();
        assert_eq!(report.code, RpcErrorCode::from("claim_required"));
        assert_eq!(report.message, "Claim this Organization to create a token.");
        assert_eq!(report.details.get("claim_url").unwrap(), claim_url);
        assert_eq!(report.details.get("action").unwrap(), "mint_token");
        assert_eq!(report.details.get("next").unwrap(), next);
        assert_eq!(failure.hints(), [Hint::Next(next.into())]);
    }

    #[test]
    fn a_sign_in_this_user_cannot_read_names_the_chmod_that_fixes_it() {
        let failure = Failure::from(LoginError::Store {
            path: "/home/dev/.config/ployz/cloud.json".into(),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        });
        assert_eq!(
            failure.hints(),
            [Hint::Next(
                "chmod 600 /home/dev/.config/ployz/cloud.json".into()
            )]
        );
    }

    #[test]
    fn a_cloud_public_error_reads_as_its_sentence_not_its_json() {
        let failure = Failure::from(LoginError::Status {
            status: 422,
            body: r#"{"_tag":"PublicError","code":"VALIDATION_FAILED","message":"Only a signed-in person can make or revoke tokens; run `ployz login`."}"#.into(),
        });
        let report = failure.report();
        assert_eq!(report.code, RpcErrorCode::InvalidArgument);
        assert_eq!(
            report.message,
            "Only a signed-in person can make or revoke tokens; run `ployz login`."
        );
    }

    #[test]
    fn a_cloud_rate_limit_reads_as_unavailable() {
        let failure = Failure::from(LoginError::Status {
            status: 429,
            body: r#"{"_tag":"PublicError","code":"RATE_LIMITED","message":"Too many failed sign-ins from this address. Try again in 42 seconds."}"#.into(),
        });
        let report = failure.report();
        assert_eq!(report.code, RpcErrorCode::Unavailable);
        assert_eq!(
            report.message,
            "Too many failed sign-ins from this address. Try again in 42 seconds."
        );
    }
}
