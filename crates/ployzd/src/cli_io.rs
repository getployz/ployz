use crate::{CliError, Result};
use ployz_api::{DaemonRequest, DaemonResponse};
use ployz_sdk::{Transport, UnixSocketTransport};
use std::io::{BufRead, BufReader, Read, Write};

pub(crate) async fn cmd_rpc_stdio(socket: &str) -> Result<i32> {
    let mut line = String::new();
    BufReader::new(std::io::stdin())
        .read_line(&mut line)
        .map_err(|error| {
            CliError::Usage(format!("failed to read daemon request from stdin: {error}"))
        })?;
    let request = serde_json::from_str::<DaemonRequest>(&line)
        .map_err(|error| CliError::Usage(format!("invalid daemon request from stdin: {error}")))?;

    let transport = UnixSocketTransport::new(socket.to_string());
    let response = request_daemon(&transport, socket, request).await?;
    let body = serde_json::to_string(&response).map_err(|error| {
        CliError::Serialize(format!("failed to encode daemon response: {error}"))
    })?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(body.as_bytes())
        .map_err(|error| CliError::Io(format!("failed to write daemon response: {error}")))?;
    stdout.write_all(b"\n").map_err(|error| {
        CliError::Io(format!("failed to write daemon response newline: {error}"))
    })?;
    stdout
        .flush()
        .map_err(|error| CliError::Io(format!("failed to flush daemon response: {error}")))?;
    Ok(0)
}

pub(crate) async fn request_daemon<T: Transport>(
    transport: &T,
    socket: &str,
    request: DaemonRequest,
) -> Result<DaemonResponse> {
    transport
        .request(request)
        .await
        .map_err(|error| CliError::Transport {
            socket: socket.to_string(),
            message: error.to_string(),
        })
}

pub(crate) fn render_response(
    json: bool,
    plain: bool,
    quiet: bool,
    response: &DaemonResponse,
) -> Result<()> {
    if json {
        let body = serde_json::to_string_pretty(response).map_err(|error| {
            CliError::Serialize(format!("failed to encode JSON output: {error}"))
        })?;
        println!("{body}");
        return Ok(());
    }

    if response.ok {
        if !quiet {
            println!("{}", response.message);
        }
        return Ok(());
    }

    if plain {
        eprintln!("{}", response.message);
    } else {
        eprintln!("error [{}]: {}", response.code, response.message);
    }
    Ok(())
}

pub(crate) fn read_stdin_string(label: &str) -> Result<String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|error| CliError::Usage(format!("failed to read {label} from stdin: {error}")))?;

    String::from_utf8(bytes).map_err(|error| {
        CliError::Usage(format!("{label} from stdin was not valid utf-8: {error}"))
    })
}

pub(crate) fn read_text_source(label: &str, path: &str) -> Result<String> {
    match path {
        "-" => read_stdin_string(label),
        other => std::fs::read_to_string(other)
            .map_err(|error| CliError::Io(format!("failed to read {label} from {other}: {error}"))),
    }
}

pub(crate) fn read_optional_text_file(
    label: &str,
    path: Option<&std::path::Path>,
) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let contents = std::fs::read_to_string(path).map_err(|error| {
        CliError::Io(format!(
            "failed to read {label} '{}': {error}",
            path.display()
        ))
    })?;
    if contents.trim().is_empty() {
        return Err(CliError::Usage(format!(
            "{label} '{}' is empty",
            path.display()
        )));
    }
    Ok(Some(contents))
}
