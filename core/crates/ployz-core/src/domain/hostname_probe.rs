//! The HTTP client that fetches [`HOSTNAME_VERIFY_PATH`] through an Ingress Hostname.

use std::{net::SocketAddr, time::Duration};

use reqwest::{Client, redirect::Policy};

use crate::{HOSTNAME_VERIFY_PATH, IngressHost, VerifyAnswer};

/// Fetches plain HTTP from one address with an Ingress Hostname as `Host`, never following a
/// redirect and never going through a proxy. The CLI's Deploy warning and a Machine's HTTP-01
/// pre-check both ask through it, so they agree on what an answer is.
#[derive(Clone, Debug)]
pub struct HostnameProbe {
    client: Client,
}

impl HostnameProbe {
    pub fn new(timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .no_proxy()
                .timeout(timeout)
                .build()?,
        })
    }

    /// The client behind the probe, for other plain-HTTP fetches through the same hostname.
    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// What `GET http://{address}` + [`HOSTNAME_VERIFY_PATH`] with `hostname` as `Host` returned.
    pub async fn answer(&self, hostname: &IngressHost, address: SocketAddr) -> VerifyAnswer {
        let Ok(response) = self
            .client
            .get(format!("http://{address}{HOSTNAME_VERIFY_PATH}"))
            .header(reqwest::header::HOST, hostname.as_str())
            .send()
            .await
        else {
            return VerifyAnswer::NoAnswer;
        };
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|location| location.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        VerifyAnswer::Answered {
            status,
            location,
            body: response.text().await.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        time::Duration,
    };

    use super::HostnameProbe;
    use crate::{IngressHost, VerifyAnswer};

    fn serve_once(response: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let request = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let read = stream.read(&mut buffer).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(buffer.get(..read).unwrap_or_default()).into_owned()
        });
        (address, request)
    }

    #[tokio::test]
    async fn answers_carry_status_location_and_body_and_the_host_header() {
        let (address, request) = serve_once(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: https://app.example.com/x\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
        );
        let probe = HostnameProbe::new(Duration::from_secs(2)).unwrap();

        let answer = probe
            .answer(&IngressHost::parse("app.example.com").unwrap(), address)
            .await;

        assert_eq!(
            answer,
            VerifyAnswer::Answered {
                status: 301,
                location: "https://app.example.com/x".into(),
                body: "hi".into(),
            }
        );
        let request = request.join().unwrap();
        assert!(request.starts_with("GET /.well-known/acme-challenge/ployz-verify HTTP/1.1"));
        assert!(request.to_lowercase().contains("host: app.example.com"));
    }

    #[tokio::test]
    async fn a_closed_port_is_no_answer() {
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let probe = HostnameProbe::new(Duration::from_secs(2)).unwrap();

        assert_eq!(
            probe
                .answer(&IngressHost::parse("app.example.com").unwrap(), address)
                .await,
            VerifyAnswer::NoAnswer
        );
    }
}
