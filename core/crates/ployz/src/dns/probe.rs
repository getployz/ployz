//! Public-IP HTTP verification, independent of DNS publication.

use super::Error;
use ployz_core::{INGRESS_VERIFY_PATH, Machine, MachineId};
use reqwest::{Client as HttpClient, redirect::Policy};
use std::{net::SocketAddr, time::Duration};

const REACHABILITY_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) async fn probe_machines(
    machines: Vec<Machine>,
) -> Result<(Vec<Machine>, Vec<String>), Error> {
    let http = HttpClient::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(REACHABILITY_TIMEOUT)
        .build()?;
    let results = futures_util::future::join_all(machines.into_iter().map(|machine| {
        let http = &http;
        async move {
            let public_ip = machine
                .public_ip
                .expect("probe candidates have a public IP");
            let port = std::env::var("PLOYZ_INGRESS_VERIFY_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(80);
            let address = SocketAddr::new(public_ip, port);
            let result = verify_machine(http, &machine.id, address)
                .await
                .map_err(|error| format!("{address}: {error}"));
            (machine, result)
        }
    }))
    .await;
    let mut reachable = Vec::new();
    let mut failures = Vec::new();
    for (machine, result) in results {
        match result {
            Ok(()) => reachable.push(machine),
            Err(error) => failures.push(format!("{}: {error}", machine.name)),
        }
    }
    if !reachable.is_empty() {
        for failure in &failures {
            eprintln!("Ingress verification: {failure}");
        }
    }
    Ok((reachable, failures))
}

#[derive(Debug, Error)]
enum ProbeError {
    #[error("{}", crate::setup_retry::detail(.0))]
    Transport(reqwest::Error),
    #[error("expected HTTP 200, received HTTP {0}")]
    Status(u16),
    #[error("response did not match the Machine ID")]
    Identity,
}

async fn verify_machine(
    http: &HttpClient,
    machine_id: &MachineId,
    address: SocketAddr,
) -> Result<(), crate::setup_retry::Error<ProbeError>> {
    crate::setup_retry::run(
        &mut (),
        &format!("Checking public ingress at {address} from this computer"),
        crate::setup_retry::WAIT,
        |error| matches!(error, ProbeError::Transport(error) if crate::setup_retry::transient_http(error)),
        async |_| probe_machine(http, machine_id, address).await,
    ).await
}

async fn probe_machine(
    http: &HttpClient,
    machine_id: &MachineId,
    address: SocketAddr,
) -> Result<(), ProbeError> {
    let response = http
        .get(format!("http://{address}{INGRESS_VERIFY_PATH}"))
        .send()
        .await
        .map_err(ProbeError::Transport)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(ProbeError::Status(status));
    }
    let body = response.bytes().await.map_err(ProbeError::Transport)?;
    if !reachability_matches(machine_id, status, Some(&body)) {
        return Err(ProbeError::Identity);
    }
    Ok(())
}

pub(super) fn reachability_matches(
    machine_id: &MachineId,
    status: u16,
    body: Option<&[u8]>,
) -> bool {
    status == 200 && body == Some(machine_id.as_str().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn ingress_probe_recovers_after_a_held_request_and_rejects_wrong_responses() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let id = MachineId::parse("a".repeat(32)).unwrap();
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_millis(100))
            .build()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_id = id;
        let server = tokio::spawn(async move {
            let (mut held, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            assert!(held.read(&mut request).await.unwrap() > 0);
            // Keep the first connection open without answering, as a held connection behaves.
            let (mut accepted, _) = listener.accept().await.unwrap();
            assert!(accepted.read(&mut request).await.unwrap() > 0);
            accepted.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: 32\r\nConnection: close\r\n\r\n{server_id}").as_bytes()).await.unwrap();
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::verify_machine(&http, &id, address),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();

        for reply in [
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nwrong",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket.write_all(reply.as_bytes()).await.unwrap();
            });
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                super::verify_machine(&http, &id, address),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(
                error.to_string().contains("403") || error.to_string().contains("Machine ID"),
                "{error}"
            );
            server.await.unwrap();
        }
    }
}
