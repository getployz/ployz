use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use reqwest::StatusCode;
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep};

use crate::error::{Error, Result};

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
}

impl ProbeRunner {
    #[must_use]
    pub fn new(docker: Docker) -> Self {
        Self { docker }
    }

    pub async fn check(&self, probe: &Probe) -> Result<bool> {
        match probe {
            Probe::Tcp { host, port } => {
                let addr = SocketAddr::new(*host, *port);
                Ok(TcpStream::connect(addr).await.is_ok())
            }
            Probe::Http { host, port, path } => {
                let url = format!("http://{host}:{port}{path}");
                let client = reqwest::Client::new();
                match client.get(url).send().await {
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
        timeout: Duration,
        interval: Duration,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.check(probe).await? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::operation(
                    "wait_ready",
                    "probe did not become ready before timeout",
                ));
            }
            sleep(interval).await;
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
