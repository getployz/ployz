use std::future::Future;

use ployz_api::frame::{OutputFrame, RequestFrame};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::DaemonControlError;

pub async fn run_rpc_stdio<R, W, H, F>(
    mut reader: R,
    mut writer: W,
    mut handler: H,
) -> Result<(), DaemonControlError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    H: FnMut(RequestFrame) -> F,
    F: Future<Output = Vec<OutputFrame>> + Send,
{
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .await
            .map_err(DaemonControlError::Read)?;
        if bytes == 0 {
            writer.flush().await.map_err(DaemonControlError::Write)?;
            return Ok(());
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        let frames = match RequestFrame::decode(trimmed) {
            Ok(request) => handler(request).await,
            Err(error) => vec![OutputFrame::error(None, error)],
        };
        for frame in frames {
            write_frame(&mut writer, &frame).await?;
        }
    }
}

async fn write_frame<W>(writer: &mut W, frame: &OutputFrame) -> Result<(), DaemonControlError>
where
    W: AsyncWrite + Unpin,
{
    let encoded = frame.encode().map_err(DaemonControlError::Encode)?;
    writer
        .write_all(encoded.as_bytes())
        .await
        .map_err(DaemonControlError::Write)?;
    writer
        .write_all(b"\n")
        .await
        .map_err(DaemonControlError::Write)?;
    writer.flush().await.map_err(DaemonControlError::Write)
}

#[cfg(test)]
mod tests {
    use ployz_api::frame::{OutputFrame, RequestFrame, ResponsePayload};
    use ployz_api::status::{
        CorrosionCleanupDto, CorrosionPhaseDto, PeerPhaseDto, SchemaPhaseDto, StartupReportDto,
        StatusResponseDto,
    };
    use tokio::io::BufReader;

    use super::run_rpc_stdio;

    #[tokio::test]
    async fn rpc_stdio_writes_response_for_valid_request() {
        let input = br#"{"id":"req-1","method":"status","payload":{}}
"#;
        let mut output = Vec::new();

        run_rpc_stdio(BufReader::new(&input[..]), &mut output, status_handler)
            .await
            .expect("rpc stdio");

        let lines = output_lines(&output);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["result"]["kind"], "status");
    }

    #[tokio::test]
    async fn rpc_stdio_malformed_line_does_not_end_session() {
        let input = br#"{ nope
{"id":"req-1","method":"status","payload":{}}
"#;
        let mut output = Vec::new();

        run_rpc_stdio(BufReader::new(&input[..]), &mut output, status_handler)
            .await
            .expect("rpc stdio");

        let lines = output_lines(&output);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], "error");
        assert_eq!(lines[0]["error"]["code"], "parse");
        assert_eq!(lines[1]["type"], "response");
    }

    #[tokio::test]
    async fn rpc_stdio_unknown_method_writes_structured_error() {
        let input = br#"{"id":"req-1","method":"deploy.apply","payload":{}}
"#;
        let mut output = Vec::new();

        run_rpc_stdio(
            BufReader::new(&input[..]),
            &mut output,
            |request| async move {
                vec![OutputFrame::error(
                    Some(request.id().clone()),
                    ployz_api::ProtocolError::unsupported_method(request.method()),
                )]
            },
        )
        .await
        .expect("rpc stdio");

        let lines = output_lines(&output);
        assert_eq!(lines[0]["type"], "error");
        assert_eq!(lines[0]["error"]["code"], "unsupported_method");
        assert_eq!(lines[0]["error"]["method"], "deploy.apply");
    }

    async fn status_handler(request: RequestFrame) -> Vec<OutputFrame> {
        let status = StatusResponseDto::new(
            true,
            true,
            Some("endpoint-1".to_owned()),
            StartupReportDto::new(
                CorrosionPhaseDto::Ready,
                CorrosionCleanupDto::NotRun,
                SchemaPhaseDto::Ready,
                PeerPhaseDto::Ready,
                "127.0.0.1:9781",
                "state/peer.key",
            ),
            None,
        );
        vec![OutputFrame::response(
            request.id().clone(),
            ResponsePayload::Status(status),
        )]
    }

    fn output_lines(output: &[u8]) -> Vec<serde_json::Value> {
        std::str::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("json line"))
            .collect()
    }
}
