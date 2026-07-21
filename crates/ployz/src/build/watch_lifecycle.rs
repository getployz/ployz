//! Foreground lifecycle policy for a persistent external Build Executor.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::{ClientError, Event, ServerError};
use ployz_core::build::BuildExecutorIdentity;
use ployz_core::nats_config::BuildExecutorCredentialExpiresAt;
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsConnectConfig, NatsTlsTrust, authenticated_connect_options,
};

use super::executor_context::{
    ExecutorIdentityPaths, load_executor_context, resolve_executor_context_root,
};
use super::runtime::BuildExecutionError;
use crate::dispatcher::PloyzctlRuntimeConfig;

const RECONNECT_DELAYS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionEvent {
    Connected,
    TransientDisconnect,
    TerminalAuthorization,
    TerminalConnection,
    Ignore,
}

pub(super) enum WatchConnect {
    Connected(WatchSession),
    Stopped,
}

pub(super) struct WatchSession {
    pub client: async_nats::Client,
    events: tokio::sync::mpsc::UnboundedReceiver<Event>,
    shutdown: Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send>>,
    runtime_state: Arc<tokio::sync::Mutex<super::external_runtime::RuntimeState>>,
    admission_generation: super::external_runtime::AdmissionGeneration,
}

pub(super) fn executor_connect_config(
    config: &PloyzctlRuntimeConfig,
    identity: &BuildExecutorIdentity,
) -> Result<(NatsConnectConfig, BuildExecutorCredentialExpiresAt), BuildExecutionError> {
    let root = resolve_executor_context_root(config.build_executor.context_root.as_deref())
        .ok_or_else(|| BuildExecutionError::ExecutorContext {
            message: "neither XDG_CONFIG_HOME nor HOME is available".to_owned(),
        })?;
    let paths = ExecutorIdentityPaths::new(&root, identity.clone());
    let inputs =
        load_executor_context(&paths).map_err(|error| BuildExecutionError::ExecutorContext {
            message: error.to_string(),
        })?;
    Ok((
        NatsConnectConfig {
            url: inputs.nats_url,
            auth: NatsClientAuth::NkeySeed(inputs.nats_seed),
            trust: NatsTlsTrust::ClusterCa(inputs.nats_ca_file),
            principal: executor_principal(identity),
        },
        inputs.credential_expires_at,
    ))
}

#[must_use]
pub(super) fn executor_principal(identity: &BuildExecutorIdentity) -> NatsPrincipal {
    NatsPrincipal::BuildExecutor {
        pool_id: identity.pool_id.clone(),
        executor_id: identity.executor_id.clone(),
    }
}

pub(super) fn resolve_workspace_root(
    explicit: Option<&Path>,
    identity: &BuildExecutorIdentity,
) -> Result<PathBuf, BuildExecutionError> {
    if let Some(explicit) = explicit {
        return Ok(explicit.to_owned());
    }
    let xdg = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let workspace = default_workspace_root(
        identity.pool_id.as_str(),
        identity.executor_id.as_str(),
        xdg.as_deref(),
        home.as_deref(),
    )
    .ok_or_else(|| BuildExecutionError::ExecutorWorkspace {
        message: "neither XDG_STATE_HOME nor HOME is available".to_owned(),
    })?;
    prepare_workspace_identity_directory(&workspace)?;
    Ok(workspace)
}

pub(super) async fn connect_watch(
    connect: &NatsConnectConfig,
    credential_expires_at: BuildExecutorCredentialExpiresAt,
    connect_timeout: Duration,
) -> Result<WatchConnect, BuildExecutionError> {
    let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let runtime_state = Arc::new(tokio::sync::Mutex::new(
        super::external_runtime::RuntimeState::new_closed(),
    ));
    let mut shutdown: Pin<Box<dyn Future<Output = Result<(), std::io::Error>> + Send>> =
        Box::pin(shutdown_signal());
    let mut attempt = 0;
    let client = loop {
        eprintln!("Build Executor connecting");
        let options = authenticated_connect_options(connect)
            .reconnect_delay_callback(reconnect_delay)
            .event_callback({
                let events_tx = events_tx.clone();
                let runtime_state = Arc::clone(&runtime_state);
                move |event| {
                    let events_tx = events_tx.clone();
                    let runtime_state = Arc::clone(&runtime_state);
                    async move {
                        match classify_event(&event) {
                            ConnectionEvent::Connected => {
                                runtime_state.lock().await.record_connected();
                            }
                            ConnectionEvent::TransientDisconnect => {
                                runtime_state.lock().await.record_disconnected();
                            }
                            ConnectionEvent::TerminalAuthorization
                            | ConnectionEvent::TerminalConnection => {
                                runtime_state.lock().await.terminate_admission();
                            }
                            ConnectionEvent::Ignore => {}
                        }
                        let _ = events_tx.send(event);
                    }
                }
            });
        let expiry =
            credential_lifetime(credential_expires_at, SystemTime::now()).map_err(|error| {
                BuildExecutionError::ExecutorCredential {
                    message: error.to_string(),
                }
            })?;
        let connection =
            tokio::time::timeout(connect_timeout, options.connect(connect.url.as_str()));
        tokio::select! {
            result = connection => match result {
                Ok(Ok(client)) => break client,
                Ok(Err(error)) if is_terminal_connect_error(&error) => {
                    return Err(BuildExecutionError::ExecutorCredential { message: error.to_string() });
                }
                Ok(Err(_)) | Err(_) => {}
            },
            signal = &mut shutdown => {
                signal.map_err(|error| BuildExecutionError::ExecutorSignal { message: error.to_string() })?;
                eprintln!("Build Executor stopping");
                eprintln!("Build Executor stopped");
                return Ok(WatchConnect::Stopped);
            }
            () = tokio::time::sleep(expiry) => return Err(expired_error(credential_expires_at)),
        }
        let delay = reconnect_delay(attempt);
        attempt = attempt.saturating_add(1);
        eprintln!("Build Executor disconnected; retrying");
        let expiry =
            credential_lifetime(credential_expires_at, SystemTime::now()).map_err(|error| {
                BuildExecutionError::ExecutorCredential {
                    message: error.to_string(),
                }
            })?;
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            signal = &mut shutdown => {
                signal.map_err(|error| BuildExecutionError::ExecutorSignal { message: error.to_string() })?;
                eprintln!("Build Executor stopping");
                eprintln!("Build Executor stopped");
                return Ok(WatchConnect::Stopped);
            }
            () = tokio::time::sleep(expiry) => return Err(expired_error(credential_expires_at)),
        }
    };
    let expiry =
        credential_lifetime(credential_expires_at, SystemTime::now()).map_err(|error| {
            BuildExecutionError::ExecutorCredential {
                message: error.to_string(),
            }
        })?;
    let expiry = tokio::time::sleep(expiry);
    tokio::pin!(expiry);
    let admission_generation = tokio::select! {
        signal = &mut shutdown => {
            signal.map_err(|error| BuildExecutionError::ExecutorSignal { message: error.to_string() })?;
            eprintln!("Build Executor stopping");
            eprintln!("Build Executor stopped");
            return Ok(WatchConnect::Stopped);
        }
        () = &mut expiry => return Err(expired_error(credential_expires_at)),
        result = wait_for_initial_connection_ack(
            &mut events,
            &runtime_state,
            connect_timeout,
        ) => {
            result?
        }
    };
    Ok(WatchConnect::Connected(WatchSession {
        client,
        events,
        shutdown,
        runtime_state,
        admission_generation,
    }))
}

async fn wait_for_initial_connection_ack(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    runtime_state: &Arc<tokio::sync::Mutex<super::external_runtime::RuntimeState>>,
    budget: Duration,
) -> Result<super::external_runtime::AdmissionGeneration, BuildExecutionError> {
    let acknowledgement = async {
        loop {
            match events.recv().await.map(|event| classify_event(&event)) {
                Some(ConnectionEvent::Connected) => {
                    if let Some(generation) = runtime_state.lock().await.admission_generation() {
                        return Ok(generation);
                    }
                }
                Some(ConnectionEvent::TransientDisconnect | ConnectionEvent::Ignore) => {}
                Some(ConnectionEvent::TerminalAuthorization) => {
                    return Err(BuildExecutionError::ExecutorCredential {
                        message: "NATS rejected Build Executor authorization".to_owned(),
                    });
                }
                Some(ConnectionEvent::TerminalConnection) => {
                    return Err(BuildExecutionError::ExecutorConnection {
                        message: "NATS closed the Build Executor connection".to_owned(),
                    });
                }
                None => {
                    return Err(BuildExecutionError::ExecutorConnection {
                        message: "NATS connection event stream closed".to_owned(),
                    });
                }
            }
        }
    };
    tokio::time::timeout(budget, acknowledgement)
        .await
        .map_err(|_| BuildExecutionError::ExecutorConnection {
            message: format!(
                "NATS connection acknowledgement timed out after {}ms",
                budget.as_millis()
            ),
        })?
}

pub(super) async fn connect_once(
    connect: &NatsConnectConfig,
    credential_expires_at: BuildExecutorCredentialExpiresAt,
    connect_timeout: Duration,
) -> Result<async_nats::Client, BuildExecutionError> {
    let expiry =
        credential_lifetime(credential_expires_at, SystemTime::now()).map_err(|error| {
            BuildExecutionError::ExecutorCredential {
                message: error.to_string(),
            }
        })?;
    let connection = authenticated_connect_options(connect).connect(connect.url.as_str());
    tokio::select! {
        biased;
        () = tokio::time::sleep(expiry) => Err(expired_error(credential_expires_at)),
        result = tokio::time::timeout(connect_timeout, connection) => match result {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(error)) if is_terminal_connect_error(&error) => {
                Err(BuildExecutionError::ExecutorCredential { message: error.to_string() })
            }
            Ok(Err(error)) => Err(BuildExecutionError::ExecutorConnection {
                message: error.to_string(),
            }),
            Err(_) => Err(BuildExecutionError::ExecutorConnection {
                message: format!("NATS connection timed out after {}ms", connect_timeout.as_millis()),
            }),
        },
    }
}

fn is_terminal_connect_error(error: &async_nats::ConnectError) -> bool {
    matches!(
        error.kind(),
        async_nats::ConnectErrorKind::Authentication
            | async_nats::ConnectErrorKind::AuthorizationViolation
    )
}

impl WatchSession {
    #[must_use]
    pub(super) fn runtime_state(
        &self,
    ) -> Arc<tokio::sync::Mutex<super::external_runtime::RuntimeState>> {
        Arc::clone(&self.runtime_state)
    }

    #[must_use]
    pub(super) fn admission_generation(&self) -> super::external_runtime::AdmissionGeneration {
        self.admission_generation
    }

    pub async fn wait(
        &mut self,
        credential_expires_at: BuildExecutorCredentialExpiresAt,
        runtime: &super::external_runtime::ExternalBuildRuntime,
        mut probe_health: impl AsyncFnMut() -> Result<String, BuildExecutionError>,
    ) -> Result<(), BuildExecutionError> {
        let expiry =
            credential_lifetime(credential_expires_at, SystemTime::now()).map_err(|error| {
                BuildExecutionError::ExecutorCredential {
                    message: error.to_string(),
                }
            })?;
        let expiry = tokio::time::sleep(expiry);
        tokio::pin!(expiry);
        let reconnect_probe = tokio::time::sleep(Duration::from_secs(86_400));
        tokio::pin!(reconnect_probe);
        let mut disconnected = false;
        loop {
            tokio::select! {
                signal = &mut self.shutdown => {
                    signal.map_err(|error| BuildExecutionError::ExecutorSignal { message: error.to_string() })?;
                    return Ok(());
                }
                () = &mut expiry => return Err(expired_error(credential_expires_at)),
                () = &mut reconnect_probe, if disconnected => {
                    let admission_generation = self.runtime_state.lock().await.admission_generation();
                    let Some(admission_generation) = admission_generation else {
                        reconnect_probe.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(1));
                        continue;
                    };
                    match probe_health().await {
                        Ok(health_description) => {
                            match runtime
                                .open_admission_before_expiry(
                                    admission_generation,
                                    credential_expires_at,
                                )
                                .await
                            {
                                Ok(true) => {
                                    if runtime
                                        .readiness_authorized_before_expiry()
                                        .await
                                    {
                                        disconnected = false;
                                        eprintln!("Build Executor {health_description}");
                                    } else {
                                        runtime.close_admission().await;
                                        credential_lifetime(
                                            credential_expires_at,
                                            SystemTime::now(),
                                        )
                                        .map_err(|_| expired_error(credential_expires_at))?;
                                        reconnect_probe.as_mut().reset(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs(1),
                                        );
                                    }
                                }
                                Ok(false) => {
                                    reconnect_probe.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(1));
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        Err(error) => {
                            eprintln!("Build Executor health probe failed: {error}");
                            reconnect_probe.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(1));
                        }
                    }
                }
                event = self.events.recv() => match event.map(|event| classify_event(&event)) {
                    Some(ConnectionEvent::TransientDisconnect) => {
                        runtime.close_admission().await;
                        disconnected = true;
                        eprintln!("Build Executor disconnected; retrying");
                    }
                    Some(ConnectionEvent::Connected) if disconnected => {
                        reconnect_probe.as_mut().reset(tokio::time::Instant::now());
                    }
                    Some(ConnectionEvent::TerminalAuthorization) => {
                        runtime.close_admission().await;
                        return Err(BuildExecutionError::ExecutorCredential {
                            message: "NATS rejected Build Executor authorization".to_owned(),
                        });
                    }
                    Some(ConnectionEvent::TerminalConnection) => {
                        runtime.close_admission().await;
                        return Err(BuildExecutionError::ExecutorConnection {
                            message: "NATS closed the Build Executor connection".to_owned(),
                        });
                    }
                    Some(ConnectionEvent::Connected | ConnectionEvent::Ignore) => {}
                    None => {
                        runtime.close_admission().await;
                        return Err(BuildExecutionError::ExecutorConnection {
                            message: "NATS connection event stream closed".to_owned(),
                        });
                    }
                }
            }
        }
    }
}

fn expired_error(credential_expires_at: BuildExecutorCredentialExpiresAt) -> BuildExecutionError {
    BuildExecutionError::ExecutorCredential {
        message: format!(
            "Build Executor credential expired at Unix timestamp {}",
            credential_expires_at.unix_seconds()
        ),
    }
}

fn prepare_workspace_identity_directory(workspace: &Path) -> Result<(), BuildExecutionError> {
    let Some(executor_directory) = workspace.parent() else {
        return Err(BuildExecutionError::ExecutorWorkspace {
            message: "workspace has no executor identity directory".to_owned(),
        });
    };
    std::fs::create_dir_all(executor_directory).map_err(|error| {
        BuildExecutionError::ExecutorWorkspace {
            message: format!("could not create {}: {error}", executor_directory.display()),
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut directory = executor_directory;
        for _ in 0..4 {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| BuildExecutionError::ExecutorWorkspace {
                    message: format!("could not protect {}: {error}", directory.display()),
                },
            )?;
            let Some(parent) = directory.parent() else {
                break;
            };
            directory = parent;
        }
    }
    Ok(())
}

#[must_use]
pub(super) fn reconnect_delay(attempt: usize) -> Duration {
    RECONNECT_DELAYS
        .get(attempt)
        .copied()
        .unwrap_or(Duration::from_secs(30))
}

#[must_use]
pub(super) fn classify_event(event: &Event) -> ConnectionEvent {
    match event {
        Event::Connected => ConnectionEvent::Connected,
        Event::Disconnected => ConnectionEvent::TransientDisconnect,
        Event::ServerError(ServerError::AuthorizationViolation) => {
            ConnectionEvent::TerminalAuthorization
        }
        Event::ServerError(ServerError::Other(message))
        | Event::ClientError(ClientError::Other(message))
            if is_authorization_error(message) =>
        {
            ConnectionEvent::TerminalAuthorization
        }
        Event::Closed
        | Event::ClientError(ClientError::MaxReconnects | ClientError::ServerNotInPool) => {
            ConnectionEvent::TerminalConnection
        }
        Event::LameDuckMode
        | Event::Draining
        | Event::SlowConsumer(_)
        | Event::ServerError(ServerError::SlowConsumer(_))
        | Event::ServerError(ServerError::Other(_))
        | Event::ClientError(ClientError::Other(_)) => ConnectionEvent::Ignore,
    }
}

#[must_use]
pub(super) fn is_authorization_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("authorization violation")
        || message.contains("permissions violation")
        || message.contains("authentication expired")
        || message.contains("authentication revoked")
        || message.contains("authorization denied")
}

pub(super) fn credential_lifetime(
    expires_at: BuildExecutorCredentialExpiresAt,
    now: SystemTime,
) -> Result<Duration, CredentialExpiryError> {
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CredentialExpiryError::ClockBeforeEpoch)?;
    Duration::from_secs(expires_at.unix_seconds())
        .checked_sub(now)
        .filter(|remaining| !remaining.is_zero())
        .ok_or(CredentialExpiryError::Expired {
            expires_at: expires_at.unix_seconds(),
        })
}

#[must_use]
pub(super) fn default_workspace_root(
    pool_id: &str,
    executor_id: &str,
    xdg_state_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let state_home = xdg_state_home
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| {
            home.filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join(".local").join("state"))
        })?;
    Some(
        state_home
            .join("ployz")
            .join("build-executors")
            .join(pool_id)
            .join(executor_id)
            .join("workspace"),
    )
}

#[cfg(unix)]
pub(super) async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        signal = terminate.recv() => signal.map(|_| ()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "SIGTERM listener closed")
        }),
    }
}

#[cfg(not(unix))]
pub(super) async fn shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum CredentialExpiryError {
    #[error("system clock is before the Unix epoch")]
    ClockBeforeEpoch,
    #[error("Build Executor credential expired at Unix timestamp {expires_at}")]
    Expired { expires_at: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backoff_caps_at_thirty_seconds() {
        let actual = (0..9).map(reconnect_delay).collect::<Vec<_>>();
        assert_eq!(
            actual,
            [1, 2, 4, 8, 16, 30, 30, 30, 30].map(Duration::from_secs)
        );
    }

    #[test]
    fn authorization_events_are_terminal_but_disconnect_is_transient() {
        assert_eq!(
            classify_event(&Event::Disconnected),
            ConnectionEvent::TransientDisconnect
        );
        assert_eq!(
            classify_event(&Event::ServerError(ServerError::AuthorizationViolation)),
            ConnectionEvent::TerminalAuthorization
        );
        assert_eq!(
            classify_event(&Event::ServerError(ServerError::Other(
                "Permissions Violation for Subscription".to_owned()
            ))),
            ConnectionEvent::TerminalAuthorization
        );
        assert_eq!(
            classify_event(&Event::Closed),
            ConnectionEvent::TerminalConnection
        );
        assert_eq!(
            classify_event(&Event::ClientError(ClientError::MaxReconnects)),
            ConnectionEvent::TerminalConnection
        );
    }

    #[test]
    fn workspace_prefers_xdg_state_and_keys_by_identity() {
        assert_eq!(
            default_workspace_root(
                "pool_a",
                "executor_b",
                Some(Path::new("/xdg")),
                Some(Path::new("/home/test")),
            ),
            Some(PathBuf::from(
                "/xdg/ployz/build-executors/pool_a/executor_b/workspace"
            ))
        );
        assert_eq!(
            default_workspace_root("pool_a", "executor_b", None, Some(Path::new("/home/test")),),
            Some(PathBuf::from(
                "/home/test/.local/state/ployz/build-executors/pool_a/executor_b/workspace"
            ))
        );
        assert_eq!(default_workspace_root("p", "e", None, None), None);
    }

    #[test]
    fn credential_expiry_fails_closed() {
        let now = UNIX_EPOCH + Duration::from_secs(100);
        let future = BuildExecutorCredentialExpiresAt::try_new(101).expect("future expiry");
        let current = BuildExecutorCredentialExpiresAt::try_new(100).expect("current expiry");
        assert_eq!(credential_lifetime(future, now), Ok(Duration::from_secs(1)));
        assert_eq!(
            credential_lifetime(current, now),
            Err(CredentialExpiryError::Expired { expires_at: 100 })
        );
    }

    #[test]
    fn credential_lifetime_preserves_subsecond_precision() {
        let expires_at = BuildExecutorCredentialExpiresAt::try_new(101).expect("future expiry");
        let now = UNIX_EPOCH + Duration::from_millis(100_750);

        assert_eq!(
            credential_lifetime(expires_at, now),
            Ok(Duration::from_millis(250))
        );
        assert_eq!(
            credential_lifetime(expires_at, UNIX_EPOCH + Duration::from_secs(101)),
            Err(CredentialExpiryError::Expired { expires_at: 101 })
        );
    }

    #[tokio::test]
    async fn initial_ack_requires_a_current_connected_event() {
        let state = Arc::new(tokio::sync::Mutex::new(
            super::super::external_runtime::RuntimeState::new_closed(),
        ));
        let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        state.lock().await.record_connected();
        let expected = state
            .lock()
            .await
            .admission_generation()
            .expect("connected generation");
        events_tx.send(Event::Connected).expect("connected event");

        assert_eq!(
            wait_for_initial_connection_ack(&mut events, &state, Duration::from_secs(1)).await,
            Ok(expected)
        );
    }

    #[tokio::test]
    async fn initial_ack_ignores_disconnect_and_reports_terminal_or_closed_stream() {
        let state = Arc::new(tokio::sync::Mutex::new(
            super::super::external_runtime::RuntimeState::new_closed(),
        ));
        let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        state.lock().await.record_connected();
        events_tx.send(Event::Connected).expect("connected event");
        state.lock().await.record_disconnected();
        events_tx
            .send(Event::Disconnected)
            .expect("disconnect event");
        drop(events_tx);
        let error = wait_for_initial_connection_ack(&mut events, &state, Duration::from_secs(1))
            .await
            .expect_err("disconnect is not an acknowledgement");
        assert!(matches!(
            error,
            BuildExecutionError::ExecutorConnection { message }
                if message == "NATS connection event stream closed"
        ));

        let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        events_tx
            .send(Event::ServerError(ServerError::AuthorizationViolation))
            .expect("terminal event");
        assert!(matches!(
            wait_for_initial_connection_ack(&mut events, &state, Duration::from_secs(1)).await,
            Err(BuildExecutionError::ExecutorCredential { .. })
        ));

        let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        events_tx.send(Event::Closed).expect("closed event");
        assert!(matches!(
            wait_for_initial_connection_ack(&mut events, &state, Duration::from_secs(1)).await,
            Err(BuildExecutionError::ExecutorConnection { message })
                if message == "NATS closed the Build Executor connection"
        ));
    }

    #[tokio::test]
    async fn initial_ack_honors_a_zero_connection_budget() {
        let state = Arc::new(tokio::sync::Mutex::new(
            super::super::external_runtime::RuntimeState::new_closed(),
        ));
        let (_events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let error = wait_for_initial_connection_ack(&mut events, &state, Duration::ZERO)
            .await
            .expect_err("zero budget times out");
        assert!(matches!(
            error,
            BuildExecutionError::ExecutorConnection { message }
                if message == "NATS connection acknowledgement timed out after 0ms"
        ));
    }
}
