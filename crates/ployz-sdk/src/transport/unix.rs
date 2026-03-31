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
    use super::*;
    use crate::transport::Transport;
    use ployz_api::{DaemonPayload, StatusPayload};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::net::UnixListener;

    fn temp_socket_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}.sock", std::process::id()))
    }

    #[tokio::test]
    async fn request_round_trips_json_over_unix_socket() {
        let path = temp_socket_path("ployz-sdk-unix");
        let listener = UnixListener::bind(&path).expect("bind unix listener");
        let response = DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "status".into(),
            payload: Some(DaemonPayload::Status(StatusPayload {
                protocol_version: 1,
                daemon_version: "0.2.2".into(),
                machine_id: "alpha".into(),
                active_network_name: Some("mesh-a".into()),
                phase: "running".into(),
                capabilities: vec!["status-payload-v1".into()],
            })),
        };

        let server = tokio::spawn({
            let response = response.clone();
            async move {
                let (stream, _) = listener.accept().await.expect("accept client");
                let (reader, mut writer) = stream.into_split();
                let mut request_line = String::new();
                let mut reader = BufReader::new(reader);
                reader
                    .read_line(&mut request_line)
                    .await
                    .expect("read request");
                let request: DaemonRequest =
                    serde_json::from_str(&request_line).expect("decode request");
                let DaemonRequest::Status = request else {
                    panic!("unexpected request: {request:?}");
                };

                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                writer
                    .write_all(encoded.as_bytes())
                    .await
                    .expect("write response");
            }
        });

        let transport = UnixSocketTransport::new(path.display().to_string());
        let received = transport
            .request(DaemonRequest::Status)
            .await
            .expect("request succeeds");
        assert!(received.ok);
        assert_eq!(received.message, "status");

        server.await.expect("server task exits");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn request_reports_invalid_json_response() {
        let path = temp_socket_path("ployz-sdk-unix-invalid");
        let listener = UnixListener::bind(&path).expect("bind unix listener");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let (_, mut writer) = stream.into_split();
            writer
                .write_all(b"not-json\n")
                .await
                .expect("write invalid response");
        });

        let transport = UnixSocketTransport::new(path.display().to_string());
        let error = transport
            .request(DaemonRequest::Status)
            .await
            .expect_err("invalid response should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        server.await.expect("server task exits");
        let _ = std::fs::remove_file(path);
    }
}
