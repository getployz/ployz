use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use backon::{ConstantBuilder, Retryable};
use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use ployz_types::error::RuntimeError;
use ployz_types::{Error, Result};
use reqwest::StatusCode;
use tokio::net::TcpStream;
use tokio::time::timeout;

pub enum Probe {
    Tcp {
        host: IpAddr,
        port: u16,
    },
    Http {
        host: IpAddr,
        port: u16,
        path: String,
    },
    Exec {
        container_id: String,
        command: Vec<String>,
    },
}

pub struct ProbeRunner {
    docker: Docker,
    http_client: reqwest::Client,
}

impl ProbeRunner {
    #[must_use]
    pub fn new(docker: Docker) -> Self {
        Self {
            docker,
            http_client: reqwest::Client::new(),
        }
    }

    pub async fn check(&self, probe: &Probe) -> Result<bool> {
        match probe {
            Probe::Tcp { host, port } => {
                let addr = SocketAddr::new(*host, *port);
                Ok(TcpStream::connect(addr).await.is_ok())
            }
            Probe::Http { host, port, path } => {
                let url = format!("http://{host}:{port}{path}");
                match self.http_client.get(url).send().await {
                    Ok(response) => Ok(response.status() == StatusCode::OK),
                    Err(_) => Ok(false),
                }
            }
            Probe::Exec {
                container_id,
                command,
            } => self.probe_exec(container_id, command).await,
        }
    }

    pub async fn wait_ready(
        &self,
        probe: &Probe,
        start_period: Duration,
        interval: Duration,
        attempt_timeout: Duration,
        retries: u32,
    ) -> Result<()> {
        let start_attempts = attempts_for_duration(start_period, interval);
        if self
            .retry_probe(probe, interval, attempt_timeout, start_attempts)
            .await?
        {
            return Ok(());
        }

        if self
            .retry_probe(probe, interval, attempt_timeout, retries as usize)
            .await?
        {
            return Ok(());
        }

        Err(Error::Runtime(RuntimeError::ProbeRetriesExhausted))
    }

    async fn retry_probe(
        &self,
        probe: &Probe,
        interval: Duration,
        attempt_timeout: Duration,
        max_attempts: usize,
    ) -> Result<bool> {
        let max_attempts = max_attempts.max(1);
        let result = (|| async { self.ready_attempt(probe, attempt_timeout).await })
            .retry(
                ConstantBuilder::default()
                    .with_delay(interval)
                    .with_max_times(max_attempts),
            )
            .when(|error| matches!(error, ProbeAttemptError::NotReady))
            .await;

        match result {
            Ok(()) => Ok(true),
            Err(ProbeAttemptError::NotReady) => Ok(false),
            Err(ProbeAttemptError::Probe(error)) => Err(error),
        }
    }

    async fn ready_attempt(
        &self,
        probe: &Probe,
        probe_timeout: Duration,
    ) -> std::result::Result<(), ProbeAttemptError> {
        if self
            .check_with_timeout(probe, probe_timeout)
            .await
            .map_err(ProbeAttemptError::Probe)?
        {
            Ok(())
        } else {
            Err(ProbeAttemptError::NotReady)
        }
    }

    async fn check_with_timeout(&self, probe: &Probe, probe_timeout: Duration) -> Result<bool> {
        match timeout(probe_timeout, self.check(probe)).await {
            Ok(result) => result,
            Err(_) => Ok(false),
        }
    }

    async fn probe_exec(&self, container_id: &str, command: &[String]) -> Result<bool> {
        let options = CreateExecOptions {
            attach_stdout: Some(false),
            attach_stderr: Some(false),
            cmd: Some(command.to_vec()),
            ..Default::default()
        };
        let exec = self
            .docker
            .create_exec(container_id, options)
            .await
            .map_err(|e| Error::operation("probe_exec", format!("create exec: {e}")))?;
        let result = self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| Error::operation("probe_exec", format!("start exec: {e}")))?;

        match result {
            StartExecResults::Attached { mut output, .. } => while output.next().await.is_some() {},
            StartExecResults::Detached => {}
        }

        let inspect = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| Error::operation("probe_exec", format!("inspect exec: {e}")))?;
        Ok(inspect.exit_code == Some(0))
    }
}

enum ProbeAttemptError {
    NotReady,
    Probe(Error),
}

fn attempts_for_duration(duration: Duration, interval: Duration) -> usize {
    if interval.is_zero() {
        return 1;
    }
    let attempts = duration.as_nanos() / interval.as_nanos() + 1;
    attempts.min(usize::MAX as u128) as usize
}

#[cfg(test)]
mod tests {
    use super::{Probe, ProbeRunner};
    use bollard::Docker;
    use ployz_types::Error;
    use ployz_types::error::RuntimeError;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    #[tokio::test]
    async fn wait_ready_returns_structured_retry_exhaustion() {
        let docker = Docker::connect_with_local_defaults().expect("Docker client should construct");
        let runner = ProbeRunner::new(docker);
        let probe = Probe::Tcp {
            host: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            port: 9,
        };

        let result = runner
            .wait_ready(
                &probe,
                Duration::ZERO,
                Duration::from_millis(1),
                Duration::from_millis(1),
                1,
            )
            .await;

        assert_eq!(
            result,
            Err(Error::Runtime(RuntimeError::ProbeRetriesExhausted))
        );
    }
}
