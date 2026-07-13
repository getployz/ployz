use super::*;
use crate::roles::gateway::projection::GatewayProjectionError;
use crate::roles::gateway::route_table::GatewayServingState;

#[test]
fn retained_last_good_attempt_keeps_steady_refresh_interval() {
    let health = Mutex::new(GatewayProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
        last_http_failure: None,
        consecutive_http_failures: 0,
        last_watch_failure: None,
        consecutive_watch_failures: 0,
        last_status_publish_failure: None,
        consecutive_status_publish_failures: 0,
    });
    let interval = Duration::from_secs(1);

    let next = record_gateway_attempt(
        &health,
        Ok(GatewayProjectorTick {
            state: crate::roles::gateway::projection::GatewayProjectionState {
                last_good: None,
                last_error: Some(GatewayProjectionError::SourceUnavailable {
                    message: "not used".to_owned(),
                }),
            },
            served: None,
            serving: GatewayServingState::LastKnownGood {
                route_count: 1,
                error: GatewayProjectionError::SourceUnavailable {
                    message: "nats unavailable".to_owned(),
                },
            },
        }),
        interval,
        Duration::from_secs(30),
    );

    assert_eq!(next, interval);
    assert_eq!(
        health
            .lock()
            .expect("gateway health lock is not poisoned")
            .last_attempt,
        Some(GatewayProcessAttempt::ServingLastKnownGood {
            route_count: 1,
            message: "SourceUnavailable { message: \"nats unavailable\" }".to_owned(),
        })
    );
}

#[test]
fn refresh_runtime_error_uses_exponential_backoff() {
    let health = Mutex::new(GatewayProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
        last_http_failure: None,
        consecutive_http_failures: 0,
        last_watch_failure: None,
        consecutive_watch_failures: 0,
        last_status_publish_failure: None,
        consecutive_status_publish_failures: 0,
    });

    let next = record_gateway_attempt(
        &health,
        Err(GatewayProcessError::RefreshTimedOut {
            timeout: Duration::from_secs(5),
        }),
        Duration::from_secs(1),
        Duration::from_secs(2),
    );

    assert_eq!(next, Duration::from_secs(4));
}

#[tokio::test]
async fn pingora_shutdown_observes_signal_sent_before_recv() {
    let (shutdown, receiver) = watch::channel(false);
    shutdown.send(true).expect("shutdown signal sends");
    let shutdown = GatewayPingoraShutdown { shutdown: receiver };

    assert!(matches!(
        shutdown.recv().await,
        ShutdownSignal::GracefulTerminate
    ));
}
