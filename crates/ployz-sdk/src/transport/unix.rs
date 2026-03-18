use ployz_api::{DaemonRequest, DaemonResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct UnixSocketTransport {
    path: String,
}

impl UnixSocketTransport {
    #[must_use]
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

impl super::Transport for UnixSocketTransport {
    async fn request(&self, request: DaemonRequest) -> std::io::Result<DaemonResponse> {
        let stream = UnixStream::connect(&self.path).await?;
        let (reader, mut writer) = stream.into_split();

        let mut line = serde_json::to_string(&request)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        line.push('\n');
        writer.write_all(line.as_bytes()).await?;
        writer.shutdown().await?;

        let mut response = String::new();
        let mut reader = BufReader::new(reader);
        reader.read_line(&mut response).await?;

        serde_json::from_str(&response)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }
}
