use crate::error::{Error, Result};
use async_trait::async_trait;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait]
pub trait ShellRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> Result<ShellOutput>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TokioShellRunner;

#[async_trait]
impl ShellRunner for TokioShellRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<ShellOutput> {
        let output = Command::new(program)
            .args(args)
            .output()
            .await
            .map_err(|err| Error::operation("shell_runner", format!("run {program}: {err}")))?;
        Ok(ShellOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}
