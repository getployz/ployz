use crate::{DaemonRequest, DaemonResponse};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct StdioTransport {
    program: OsString,
    args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
}

impl StdioTransport {
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.envs(&self.env);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command
    }

    #[must_use]
    pub fn command_display(&self) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(shell_render(&self.program));
        parts.extend(self.args.iter().map(|arg| shell_render(arg)));
        parts.join(" ")
    }
}

impl super::Transport for StdioTransport {
    async fn request(&self, req: DaemonRequest) -> std::io::Result<DaemonResponse> {
        let mut child = self.command().spawn()?;
        let Some(mut stdin) = child.stdin.take() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "child stdin unavailable",
            ));
        };

        let mut line = serde_json::to_string(&req)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);

        let Some(stdout) = child.stdout.take() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "child stdout unavailable",
            ));
        };
        let Some(stderr) = child.stderr.take() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "child stderr unavailable",
            ));
        };

        let mut stdout = BufReader::new(stdout);
        let mut response_line = String::new();
        stdout.read_line(&mut response_line).await?;
        let mut stderr = stderr;
        let mut stderr_bytes = Vec::new();
        stderr.read_to_end(&mut stderr_bytes).await?;
        let status = child.wait().await?;

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
            let detail = if stderr.is_empty() {
                format!(
                    "command exited with status {}",
                    status.code().unwrap_or_default()
                )
            } else {
                stderr
            };
            return Err(std::io::Error::other(detail));
        }

        serde_json::from_str(&response_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

fn shell_render(value: &OsStr) -> String {
    let rendered = value.to_string_lossy();
    if rendered
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
    {
        return rendered.into_owned();
    }
    format!("'{}'", rendered.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::StdioTransport;
    use crate::transport::Transport;
    use crate::DaemonRequest;

    #[tokio::test]
    async fn stdio_transport_round_trip_reads_and_writes_line_protocol() {
        let transport = StdioTransport::new("/bin/sh").args([
            "-c",
            "read line\ncase \"$line\" in\n  *Status*) printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"pong\",\"payload\":null}\\n' ;;\n  *) printf '{\"ok\":false,\"code\":\"BAD\",\"message\":\"unexpected\",\"payload\":null}\\n' ;;\nesac\n",
        ]);

        let response = transport
            .request(DaemonRequest::Status)
            .await
            .expect("request over stdio");
        assert!(response.ok);
        assert_eq!(response.message, "pong");
    }
}
