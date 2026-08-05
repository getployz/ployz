//! API-side client for Keeper's point-in-time handshake observation.

use std::path::Path;

use ployz_core::network::WireGuardPublicKey;
use ployz_core::{
    HandshakeObservation, HandshakeObservationOutcome, HandshakeObservationUnavailable,
};
use tokio::sync::watch;

use crate::roles::handshake_control::{
    HandshakeControlProtocolError, HandshakeControlRequest, HandshakeControlResponse,
    HandshakeControlUnavailable, request,
};

pub(crate) async fn observe_handshake(
    socket_path: &Path,
    public_key: WireGuardPublicKey,
    shutdown: watch::Receiver<bool>,
) -> HandshakeObservationOutcome {
    match request(
        socket_path,
        &HandshakeControlRequest::ObserveHandshake { public_key },
        shutdown,
    )
    .await
    {
        Ok(HandshakeControlResponse::Observed {
            observed_at,
            age_seconds: Some(age_seconds),
        }) => HandshakeObservationOutcome::Observed {
            observation: HandshakeObservation::Ago {
                observed_at,
                age_seconds,
            },
        },
        Ok(HandshakeControlResponse::Observed {
            observed_at,
            age_seconds: None,
        }) => HandshakeObservationOutcome::Observed {
            observation: HandshakeObservation::Never { observed_at },
        },
        Ok(HandshakeControlResponse::Unavailable { reason }) => unavailable(match reason {
            HandshakeControlUnavailable::PeerAbsent => HandshakeObservationUnavailable::PeerAbsent,
            HandshakeControlUnavailable::UnsupportedProvider => {
                HandshakeObservationUnavailable::UnsupportedProvider
            }
            HandshakeControlUnavailable::ProviderTimedOut => {
                HandshakeObservationUnavailable::KeeperTimedOut
            }
            HandshakeControlUnavailable::ProviderFailed => {
                HandshakeObservationUnavailable::KeeperUnavailable
            }
            HandshakeControlUnavailable::Protocol => {
                HandshakeObservationUnavailable::KeeperProtocol
            }
        }),
        Err(HandshakeControlProtocolError::Deadline { .. }) => {
            unavailable(HandshakeObservationUnavailable::KeeperTimedOut)
        }
        Err(
            HandshakeControlProtocolError::Decode { .. }
            | HandshakeControlProtocolError::EmptyMessage
            | HandshakeControlProtocolError::TooLarge { .. }
            | HandshakeControlProtocolError::Encode { .. },
        ) => unavailable(HandshakeObservationUnavailable::KeeperProtocol),
        Err(
            HandshakeControlProtocolError::Connect { .. }
            | HandshakeControlProtocolError::Read { .. }
            | HandshakeControlProtocolError::Write { .. }
            | HandshakeControlProtocolError::Shutdown,
        ) => unavailable(HandshakeObservationUnavailable::KeeperUnavailable),
    }
}

fn unavailable(reason: HandshakeObservationUnavailable) -> HandshakeObservationOutcome {
    HandshakeObservationOutcome::Unavailable { reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::handshake_control::{
        CONTROL_REQUEST_TIMEOUT, HandshakeControlResponse, HandshakeControlUnavailable,
        MAX_CONTROL_MESSAGE_BYTES, read_request, write_response,
    };
    use ployz_core::corrosion::CorrosionTimestamp;
    use tokio::io::AsyncWriteExt;

    fn key() -> WireGuardPublicKey {
        WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("public key")
    }

    async fn response_from_server(
        response: HandshakeControlResponse,
    ) -> HandshakeObservationOutcome {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("keeper-control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("client");
            let _request = read_request(&mut stream).await.expect("request");
            write_response(&mut stream, &response)
                .await
                .expect("response");
        });
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let outcome = observe_handshake(&socket, key(), shutdown_rx).await;
        server.await.expect("server task");
        outcome
    }

    #[tokio::test]
    async fn observed_age_and_never_map_without_fabrication() {
        let timestamp = CorrosionTimestamp::try_new("2026-08-05T12:00:00Z").expect("timestamp");
        assert_eq!(
            response_from_server(HandshakeControlResponse::Observed {
                observed_at: timestamp,
                age_seconds: Some(17),
            })
            .await,
            HandshakeObservationOutcome::Observed {
                observation: HandshakeObservation::Ago {
                    observed_at: timestamp,
                    age_seconds: 17,
                }
            }
        );
        assert_eq!(
            response_from_server(HandshakeControlResponse::Observed {
                observed_at: timestamp,
                age_seconds: None,
            })
            .await,
            HandshakeObservationOutcome::Observed {
                observation: HandshakeObservation::Never {
                    observed_at: timestamp,
                }
            }
        );
    }

    #[tokio::test]
    async fn keeper_unavailable_reasons_map_exhaustively() {
        let cases = [
            (
                HandshakeControlUnavailable::PeerAbsent,
                HandshakeObservationUnavailable::PeerAbsent,
            ),
            (
                HandshakeControlUnavailable::UnsupportedProvider,
                HandshakeObservationUnavailable::UnsupportedProvider,
            ),
            (
                HandshakeControlUnavailable::ProviderFailed,
                HandshakeObservationUnavailable::KeeperUnavailable,
            ),
            (
                HandshakeControlUnavailable::ProviderTimedOut,
                HandshakeObservationUnavailable::KeeperTimedOut,
            ),
            (
                HandshakeControlUnavailable::Protocol,
                HandshakeObservationUnavailable::KeeperProtocol,
            ),
        ];
        for (control_reason, expected) in cases {
            assert_eq!(
                response_from_server(HandshakeControlResponse::Unavailable {
                    reason: control_reason,
                })
                .await,
                unavailable(expected)
            );
        }
    }

    #[tokio::test]
    async fn malformed_and_oversized_keeper_responses_are_protocol_unavailable() {
        for response in [vec![b'{'], vec![b'x'; MAX_CONTROL_MESSAGE_BYTES + 1]] {
            let directory = tempfile::tempdir().expect("temporary directory");
            let socket = directory.path().join("keeper-control.sock");
            let listener = tokio::net::UnixListener::bind(&socket).expect("listener");
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("client");
                let _request = read_request(&mut stream).await.expect("request");
                stream.write_all(&response).await.expect("raw response");
            });
            let (_shutdown_tx, shutdown_rx) = watch::channel(false);
            assert_eq!(
                observe_handshake(&socket, key(), shutdown_rx).await,
                unavailable(HandshakeObservationUnavailable::KeeperProtocol)
            );
            server.await.expect("server task");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn keeper_timeout_and_shutdown_are_explicitly_unavailable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("keeper-control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("listener");
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("client");
            std::future::pending::<()>().await;
        });
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let client = tokio::spawn({
            let socket = socket.clone();
            async move { observe_handshake(&socket, key(), shutdown_rx).await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(CONTROL_REQUEST_TIMEOUT).await;
        assert_eq!(
            client.await.expect("client task"),
            unavailable(HandshakeObservationUnavailable::KeeperTimedOut)
        );
        server.abort();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        shutdown_tx.send(true).expect("shutdown");
        assert_eq!(
            observe_handshake(&socket, key(), shutdown_rx).await,
            unavailable(HandshakeObservationUnavailable::KeeperUnavailable)
        );
    }
}
