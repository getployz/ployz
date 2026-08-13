use std::{net::SocketAddr, path::PathBuf};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::UnixStream;
use tokio_util::codec::LengthDelimitedCodec;

use super::Error;
use ployz_core::MembershipObservation;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipState {
    pub address: SocketAddr,
    pub membership: MembershipObservation,
}

#[derive(Clone, Debug)]
pub struct AdminClient {
    socket_path: PathBuf,
}

impl AdminClient {
    #[must_use]
    pub(crate) fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    async fn command(&self, command: &impl Serialize) -> Result<Vec<Value>, Error> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let mut framed = LengthDelimitedCodec::builder()
            .big_endian()
            .length_field_length(4)
            .new_framed(stream);
        framed
            .send(Bytes::from(serde_json::to_vec(command)?))
            .await?;

        let mut values = Vec::new();
        while let Some(frame) = framed.next().await {
            let frame = frame.map_err(|error| Error::Protocol(error.to_string()))?;
            let response = decode_response(&frame)?;
            match response {
                AdminResponse::Success => return Ok(values),
                AdminResponse::Json(value) => values.push(value),
                AdminResponse::Error { msg } => return Err(Error::Api(msg)),
                AdminResponse::Log { .. } => {}
            }
        }
        Err(Error::Protocol(
            "admin response ended before Success".into(),
        ))
    }

    pub async fn membership_states(&self) -> Result<Vec<MembershipState>, Error> {
        self.command(&serde_json::json!({"Cluster": "MembershipStates"}))
            .await?
            .into_iter()
            .map(decode_membership_state)
            .collect()
    }
}

fn decode_membership_state(value: Value) -> Result<MembershipState, Error> {
    #[derive(Deserialize)]
    struct RawState {
        id: RawId,
        state: String,
    }

    #[derive(Deserialize)]
    struct RawId {
        addr: SocketAddr,
    }

    let raw: RawState = serde_json::from_value(value)?;
    let membership = match raw.state.as_str() {
        "Alive" => MembershipObservation::Up,
        "Suspect" => MembershipObservation::Suspect,
        _ => MembershipObservation::Down,
    };
    Ok(MembershipState {
        address: raw.id.addr,
        membership,
    })
}

fn decode_response(frame: &[u8]) -> Result<AdminResponse, Error> {
    serde_json::from_slice(frame).map_err(|error| Error::Protocol(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum AdminResponse {
    Log {
        #[serde(rename = "level")]
        _level: String,
        #[serde(rename = "msg")]
        _msg: String,
        #[serde(rename = "ts")]
        _ts: Value,
    },
    Error {
        msg: String,
    },
    Success,
    Json(Value),
}

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };

    use super::{AdminClient, decode_membership_state, decode_response};
    use crate::corrosion::Error;

    #[test]
    fn malformed_response_is_a_protocol_error() {
        assert!(matches!(
            decode_response(b"not json"),
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn membership_state_is_decoded_at_the_admin_boundary() {
        let state = decode_membership_state(json!({
            "id": {"addr": "[fdcc::1]:51001"},
            "state": "Alive"
        }))
        .unwrap();
        assert_eq!(state.address, "[fdcc::1]:51001".parse().unwrap());
        assert_eq!(state.membership, ployz_core::MembershipObservation::Up);
    }

    #[tokio::test]
    async fn client_uses_big_endian_length_framed_json() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-corrosion-admin-{}",
            ployz_core::MachineId::random()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("admin.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let command = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&command).unwrap(),
                json!({"Cluster": "MembershipStates"})
            );
            write_frame(&mut stream, br#"{"Json":{"state":"Alive"}}"#)
                .await
                .unwrap();
            write_frame(&mut stream, br#""Success""#).await.unwrap();
        });

        let response = AdminClient::new(path)
            .command(&json!({"Cluster": "MembershipStates"}))
            .await
            .unwrap();
        assert_eq!(response, vec![json!({"state": "Alive"})]);
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn read_frame(stream: &mut tokio::net::UnixStream) -> io::Result<Vec<u8>> {
        let length = stream.read_u32().await?;
        let mut data = vec![0; length as usize];
        stream.read_exact(&mut data).await?;
        Ok(data)
    }

    async fn write_frame(stream: &mut tokio::net::UnixStream, data: &[u8]) -> io::Result<()> {
        stream.write_u32(data.len() as u32).await?;
        stream.write_all(data).await
    }
}
