use serde_json::Value;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipState {
    Alive,
    Suspect,
    Down,
}

impl MembershipState {
    #[must_use]
    pub fn is_active(self) -> bool {
        match self {
            Self::Alive | Self::Suspect => true,
            Self::Down => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterMembershipState {
    pub addr: SocketAddr,
    pub id: String,
    pub state: MembershipState,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct AdminClient {
    sock_path: PathBuf,
}

impl AdminClient {
    #[must_use]
    pub fn new(sock_path: impl Into<PathBuf>) -> Self {
        Self {
            sock_path: sock_path.into(),
        }
    }

    pub async fn cluster_membership_states_latest(
        &self,
    ) -> Result<Vec<ClusterMembershipState>, AdminError> {
        let mut stream = UnixStream::connect(&self.sock_path).await?;
        stream
            .write_all(&encode_frame(br#"{"Cluster":"MembershipStates"}"#))
            .await?;

        let mut latest = BTreeMap::<String, ClusterMembershipState>::new();
        loop {
            let frame = read_frame(&mut stream).await?;
            match serde_json::from_slice::<Value>(&frame)? {
                Value::String(success) if success == "Success" => {
                    return Ok(latest.into_values().collect());
                }
                Value::Object(mut object) => {
                    if let Some(error) = object.remove("Error") {
                        return Err(AdminError::Response(parse_error_message(&error)));
                    }
                    let Some(json) = object.remove("Json") else {
                        continue;
                    };
                    let state = parse_cluster_membership_state(&json)?;
                    match latest.get(&state.id) {
                        Some(existing) if existing.timestamp >= state.timestamp => {}
                        Some(_) | None => {
                            latest.insert(state.id.clone(), state);
                        }
                    }
                }
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_) => {}
            }
        }
    }
}

fn encode_frame(data: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(4 + data.len());
    encoded.extend_from_slice(&(data.len() as u32).to_be_bytes());
    encoded.extend_from_slice(data);
    encoded
}

async fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, AdminError> {
    let mut head = [0_u8; 4];
    stream.read_exact(&mut head).await?;
    let length = u32::from_be_bytes(head) as usize;
    let mut data = vec![0_u8; length];
    stream.read_exact(&mut data).await?;
    Ok(data)
}

fn parse_error_message(error: &Value) -> String {
    let Value::Object(object) = error else {
        return format!("unexpected admin error payload: {error}");
    };
    let Some(Value::String(message)) = object.get("msg") else {
        return format!("unexpected admin error payload: {error}");
    };
    message.clone()
}

fn parse_cluster_membership_state(value: &Value) -> Result<ClusterMembershipState, AdminError> {
    let Value::Object(object) = value else {
        return Err(AdminError::InvalidPayload(format!(
            "membership payload is not an object: {value}"
        )));
    };
    let Some(Value::Object(id_object)) = object.get("id") else {
        return Err(AdminError::InvalidPayload(format!(
            "membership payload missing id object: {value}"
        )));
    };
    let Some(Value::String(id)) = id_object.get("id") else {
        return Err(AdminError::InvalidPayload(format!(
            "membership payload missing actor id: {value}"
        )));
    };
    let Some(Value::String(addr)) = id_object.get("addr") else {
        return Err(AdminError::InvalidPayload(format!(
            "membership payload missing addr: {value}"
        )));
    };
    let addr = addr.parse().map_err(|error| {
        AdminError::InvalidPayload(format!("invalid membership addr '{addr}': {error}"))
    })?;
    let Some(timestamp) = id_object.get("ts").and_then(Value::as_u64).or_else(|| {
        id_object
            .get("ts")
            .and_then(Value::as_f64)
            .map(|value| value as u64)
    }) else {
        return Err(AdminError::InvalidPayload(format!(
            "membership payload missing timestamp: {value}"
        )));
    };
    let Some(Value::String(state)) = object.get("state") else {
        return Err(AdminError::InvalidPayload(format!(
            "membership payload missing state: {value}"
        )));
    };
    let state = match state.as_str() {
        "Alive" => MembershipState::Alive,
        "Suspect" => MembershipState::Suspect,
        "Down" => MembershipState::Down,
        other => {
            return Err(AdminError::InvalidPayload(format!(
                "unknown membership state '{other}'"
            )));
        }
    };

    Ok(ClusterMembershipState {
        addr,
        id: id.clone(),
        state,
        timestamp,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Response(String),
    #[error("{0}")]
    InvalidPayload(String),
}

#[cfg(test)]
mod tests {
    use super::{MembershipState, parse_cluster_membership_state};
    use serde_json::json;

    #[test]
    fn parses_membership_state() {
        let payload = json!({
            "id": {
                "addr": "[fd00::1]:51001",
                "id": "actor-1",
                "ts": 123_u64,
            },
            "state": "Suspect",
        });

        let state = parse_cluster_membership_state(&payload).expect("parse membership state");
        assert_eq!(state.id, "actor-1");
        assert_eq!(state.state, MembershipState::Suspect);
        assert_eq!(state.timestamp, 123);
    }
}
