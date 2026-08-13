use std::path::{Path, PathBuf};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::net::UnixStream;
use tokio_util::codec::LengthDelimitedCodec;

use super::Error;

#[derive(Clone, Debug)]
pub struct AdminClient {
    socket_path: PathBuf,
}

impl AdminClient {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn command(&self, command: &impl Serialize) -> Result<Vec<Value>, Error> {
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
            match serde_json::from_slice::<Value>(&frame?)? {
                Value::String(value) if value == "Success" => return Ok(values),
                Value::Object(mut object) => {
                    if let Some(error) = object.remove("Error") {
                        let message = error
                            .get("msg")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown admin error");
                        return Err(Error::Api(message.to_owned()));
                    }
                    if let Some(value) = object.remove("Json") {
                        values.push(value);
                    }
                }
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_) => {}
            }
        }
        Err(Error::Protocol(
            "admin response ended before Success".into(),
        ))
    }
}
