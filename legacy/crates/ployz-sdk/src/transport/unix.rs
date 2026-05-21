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

#[cfg(test)]
mod tests {
    use super::UnixSocketTransport;
    use crate::transport::Transport;
    use ployz_api::DaemonRequest;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn unix_transport_round_trip_reads_and_writes_line_protocol() {
        let socket = test_socket_path("round-trip");
        let listener = UnixListener::bind(&socket).expect("bind test socket");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept request");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut request = String::new();
            reader.read_line(&mut request).await.expect("read request");
            assert!(request.contains("Status"));
            writer
                .write_all(b"{\"status\":\"success\",\"code\":\"OK\",\"message\":\"pong\"}\n")
                .await
                .expect("write response");
        });

        let response = UnixSocketTransport::new(socket.to_string_lossy().into_owned())
            .request(DaemonRequest::Status)
            .await
            .expect("request over unix socket");

        assert!(response.is_ok());
        assert_eq!(response.message(), "pong");
        server.await.expect("server task");
        let _ = std::fs::remove_file(socket);
    }

    #[tokio::test]
    async fn unix_transport_surfaces_malformed_daemon_output() {
        let socket = test_socket_path("malformed");
        let listener = UnixListener::bind(&socket).expect("bind test socket");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept request");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut request = String::new();
            reader.read_line(&mut request).await.expect("read request");
            writer
                .write_all(b"not-json\n")
                .await
                .expect("write response");
        });

        let error = UnixSocketTransport::new(socket.to_string_lossy().into_owned())
            .request(DaemonRequest::Status)
            .await
            .expect_err("malformed daemon output should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        server.await.expect("server task");
        let _ = std::fs::remove_file(socket);
    }

    fn test_socket_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ployz-sdk-{name}-{}-{nanos}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }
}
