use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    Status,
    MeshCreate { network: String },
    MeshInit { network: String },
    MeshUp { network: String },
    MeshDown,
    MeshDestroy { network: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    pub message: String,
}

pub struct UnixSocketTransport {
    path: String,
}

impl UnixSocketTransport {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

impl super::Transport for UnixSocketTransport {
    async fn request(&self, req: DaemonRequest) -> std::io::Result<DaemonResponse> {
        let stream = UnixStream::connect(&self.path).await?;
        let (reader, mut writer) = stream.into_split();

        let mut line = serde_json::to_string(&req)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        writer.write_all(line.as_bytes()).await?;
        writer.shutdown().await?;

        let mut buf_reader = BufReader::new(reader);
        let mut response_line = String::new();
        buf_reader.read_line(&mut response_line).await?;

        serde_json::from_str(&response_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
