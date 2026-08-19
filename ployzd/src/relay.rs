//! Cloud Relay Register client.

use std::time::Duration;

use ployz_core::MachineId;
use ployz_relay::{AUTHORIZATION_METADATA, CloudRelayClient, PairingCredential, RegisterRequest};
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{
    Request,
    metadata::MetadataValue,
    transport::{Channel, Endpoint},
};

/// Held Cloud Relay Register. Dropping it ends the stream.
#[must_use]
pub struct RegisterHold {
    task: JoinHandle<()>,
}

/// Failures holding Cloud Relay Register.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Hold Register: hello with Machine ID, Pairing Credential bearer, echo pings.
///
/// # Errors
///
/// If the Relay is unreachable or Register is rejected.
pub async fn hold_register(
    url: &str,
    pairing: &PairingCredential,
    machine_id: &MachineId,
) -> Result<RegisterHold, Error> {
    let mut relay = CloudRelayClient::new(connect(url).await?);
    let (tx, rx) = mpsc::channel(4);
    tx.send(RegisterRequest::new(machine_id))
        .await
        .expect("hello is sent before the hold task starts");
    let mut request = Request::new(ReceiverStream::new(rx));
    set_bearer(request.metadata_mut(), pairing);
    let mut opens = relay.register(request).await?.into_inner();
    let task = tokio::spawn(async move {
        while let Some(Ok(message)) = opens.next().await {
            // ponytail: Attach is #408
            if let Some(nonce) = message.ping_nonce() {
                let _ = tx.send(RegisterRequest::pong(nonce)).await;
            }
        }
    });
    Ok(RegisterHold { task })
}

impl Drop for RegisterHold {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn connect(url: &str) -> Result<Channel, tonic::transport::Error> {
    Endpoint::from_shared(url.to_owned())?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
}

fn set_bearer(metadata: &mut tonic::metadata::MetadataMap, pairing: &PairingCredential) {
    metadata.insert(
        AUTHORIZATION_METADATA,
        MetadataValue::try_from(format!("Bearer {}", pairing.as_str()))
            .expect("Pairing Credential is ASCII metadata"),
    );
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use ployz_core::MachineId;
    use ployz_relay::{
        AUTHORIZATION_METADATA, CloudRelayClient, DialCredential, ListRequest, MACHINE_ID_METADATA,
        PAIRING_METADATA, PairingCredential, Relay, RevokeRequest,
    };
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tonic::{Request, metadata::MetadataValue, transport::Endpoint};

    use super::{Error, hold_register};

    const PAIRING: &str = "pairing-secret";
    const DIAL: &str = "dial-secret";
    const OTHER: &str = "other-pairing";

    #[tokio::test]
    async fn register_hello_appears_on_list_held() {
        let session = Session::start().await;
        let listed = list(&session.url, DIAL, PAIRING).await.unwrap();
        let [row] = listed.as_slice() else {
            panic!("expected one held Register, got {listed:?}");
        };
        assert_eq!(row.machine_id().unwrap(), session.machine_id);
    }

    #[tokio::test]
    async fn register_pings_are_echoed() {
        let session = Session::start().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let listed = list(&session.url, DIAL, PAIRING).await.unwrap();
            if listed.iter().any(|row| {
                row.machine_id().ok() == Some(session.machine_id) && row.register_rtt_ns.is_some()
            }) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "path RTT must be present after a pong"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn pairing_credential_cannot_impersonate_cloud() {
        let session = Session::start().await;
        let mut cloud = client(&session.url).await;

        let list = cloud
            .list(cloud_request(ListRequest {}, PAIRING, PAIRING, None))
            .await;
        assert_eq!(list.unwrap_err().code(), tonic::Code::Unauthenticated);

        let revoke = cloud
            .revoke(cloud_request(RevokeRequest {}, PAIRING, PAIRING, None))
            .await;
        assert_eq!(revoke.unwrap_err().code(), tonic::Code::Unauthenticated);

        let dial = cloud
            .dial(cloud_request(
                ReceiverStream::new(mpsc::channel(1).1),
                PAIRING,
                PAIRING,
                Some(&session.machine_id),
            ))
            .await;
        assert_eq!(dial.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[tokio::test]
    async fn wrong_pairing_does_not_list_this_machine() {
        let session = Session::start().await;
        let listed = list(&session.url, DIAL, OTHER).await.unwrap();
        assert!(
            listed
                .iter()
                .all(|row| row.machine_id().ok() != Some(session.machine_id))
        );
    }

    #[tokio::test]
    async fn unreachable_relay_fails() {
        let pairing = PairingCredential::parse(PAIRING).unwrap();
        let error = match hold_register("not-a-url", &pairing, &MachineId::random()).await {
            Ok(_) => panic!("expected unreachable Relay to fail"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::Transport(_)));
    }

    struct Session {
        url: String,
        machine_id: MachineId,
        _hold: super::RegisterHold,
        _server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
        _goaway: ployz_relay::Goaway,
    }

    impl Session {
        async fn start() -> Self {
            let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
            let listen = (Ipv4Addr::LOCALHOST, 0).into();
            let (address, server, goaway) = relay.serve(listen).await.unwrap();
            let url = format!("http://{address}");
            let machine_id = MachineId::random();
            let pairing = PairingCredential::parse(PAIRING).unwrap();
            let hold = hold_register(&url, &pairing, &machine_id)
                .await
                .expect("Register hello is accepted");
            Self {
                url,
                machine_id,
                _hold: hold,
                _server: server,
                _goaway: goaway,
            }
        }
    }

    async fn list(
        url: &str,
        dial: &str,
        pairing: &str,
    ) -> Result<Vec<ployz_relay::HeldRegister>, tonic::Status> {
        client(url)
            .await
            .list(cloud_request(ListRequest {}, dial, pairing, None))
            .await
            .map(|response| response.into_inner().registers().to_vec())
    }

    async fn client(url: &str) -> CloudRelayClient<tonic::transport::Channel> {
        let channel = Endpoint::from_shared(url.to_owned())
            .unwrap()
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await
            .unwrap();
        CloudRelayClient::new(channel)
    }

    fn cloud_request<T>(
        body: T,
        bearer: &str,
        pairing: &str,
        machine_id: Option<&MachineId>,
    ) -> Request<T> {
        let mut request = Request::new(body);
        request.metadata_mut().insert(
            AUTHORIZATION_METADATA,
            MetadataValue::try_from(format!("Bearer {bearer}")).expect("bearer is ASCII"),
        );
        request.metadata_mut().insert(
            PAIRING_METADATA,
            pairing.parse().expect("pairing is ASCII metadata"),
        );
        if let Some(machine_id) = machine_id {
            request.metadata_mut().insert(
                MACHINE_ID_METADATA,
                machine_id
                    .as_str()
                    .parse()
                    .expect("Machine ID is ASCII metadata"),
            );
        }
        request
    }
}
