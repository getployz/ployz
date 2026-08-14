use ployz_core::{DnsRecord, DnsRecordRequest};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// TODO: Replace dns.uncloud.run and Uncloud-branded domains with
// Ployz-hosted DNS once that infrastructure exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Reservation {
    pub(crate) endpoint: String,
    pub(crate) name: String,
    // TODO(UT-141): encrypt the token in the store.
    pub(crate) token: String,
}

pub(crate) struct HostedDnsClient {
    client: Client,
}

impl HostedDnsClient {
    pub(crate) fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub(crate) async fn reserve(&self, endpoint: &str) -> Result<Reservation, Error> {
        let response = self
            .client
            .post(endpoint_url(endpoint, &["domains"])?)
            .send()
            .await?;
        let response: DomainResponse = decode(response).await?;
        Ok(Reservation {
            endpoint: endpoint.to_owned(),
            name: response.name,
            token: response.token,
        })
    }

    pub(crate) async fn create_records(
        &self,
        reservation: &Reservation,
        records: &[DnsRecordRequest],
    ) -> Result<Vec<DnsRecord>, Error> {
        let url = endpoint_url(
            &reservation.endpoint,
            &["domains", &reservation.name, "records"],
        )?;
        let mut created = Vec::with_capacity(records.len());
        for record in records {
            let response = self
                .client
                .post(url.clone())
                .bearer_auth(&reservation.token)
                .json(record)
                .send()
                .await?;
            let response: RecordResponse = decode(response).await?;
            created.push(DnsRecord {
                name: response.fqdn,
                record_type: response.record_type,
                values: response.values,
            });
        }
        Ok(created)
    }
}

fn endpoint_url(endpoint: &str, segments: &[&str]) -> Result<Url, Error> {
    let mut url =
        Url::parse(endpoint).map_err(|error| Error::InvalidEndpoint(error.to_string()))?;
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| Error::InvalidEndpoint("endpoint cannot be a URL base".into()))?;
    path.pop_if_empty();
    path.extend(segments);
    drop(path);
    Ok(url)
}

async fn decode<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T, Error> {
    let status = response.status();
    let body = response.bytes().await?;
    if status == StatusCode::UNAUTHORIZED {
        let error: AuthError = serde_json::from_slice(&body)?;
        return Err(if error.data.no_domain {
            Error::AuthNoDomain
        } else {
            Error::Authentication
        });
    }
    if !status.is_success() {
        return Err(Error::Status(status.as_u16()));
    }
    Ok(serde_json::from_slice(&body)?)
}

#[derive(Deserialize)]
struct DomainResponse {
    name: String,
    token: String,
}

#[derive(Deserialize)]
struct RecordResponse {
    #[serde(rename = "type")]
    record_type: ployz_core::DnsRecordType,
    values: Vec<String>,
    fqdn: String,
}

#[derive(Deserialize)]
struct AuthError {
    data: AuthErrorData,
}

#[derive(Deserialize)]
struct AuthErrorData {
    #[serde(rename = "noDomain")]
    no_domain: bool,
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("hosted DNS request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("hosted DNS response was invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid hosted DNS endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("hosted DNS returned HTTP {0}")]
    Status(u16),
    #[error("hosted DNS authentication failed")]
    Authentication,
    #[error("the supplied domain failed authentication")]
    AuthNoDomain,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ployz_core::{DnsRecordRequest, DnsRecordType};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::{Error, HostedDnsClient};

    #[derive(Debug)]
    struct Request {
        head: String,
        body: Vec<u8>,
    }

    #[tokio::test]
    async fn hosted_requests_keep_the_retained_wire_contract_exact() {
        let responses = [
            (
                200,
                r#"{"name":"opaque.ployz.example","token":"raw-token"}"#,
            ),
            (
                200,
                r#"{"name":"*","type":"A","values":["192.0.2.1"],"fqdn":"*.opaque.ployz.example"}"#,
            ),
            (
                200,
                r#"{"name":"*","type":"AAAA","values":["2001:db8::1"],"fqdn":"*.opaque.ployz.example"}"#,
            ),
        ];
        let (endpoint, requests) = fake_server(responses).await;
        let client = HostedDnsClient::new();

        let reservation = client.reserve(&endpoint).await.unwrap();
        assert_eq!(reservation.name, "opaque.ployz.example");
        assert_eq!(reservation.token, "raw-token");
        let records = client
            .create_records(
                &reservation,
                &[
                    DnsRecordRequest {
                        name: "*".into(),
                        record_type: DnsRecordType::A,
                        values: vec!["192.0.2.1".into()],
                    },
                    DnsRecordRequest {
                        name: "*".into(),
                        record_type: DnsRecordType::Aaaa,
                        values: vec!["2001:db8::1".into()],
                    },
                ],
            )
            .await
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records.first().unwrap().name, "*.opaque.ployz.example");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let requests = requests.lock().unwrap();
        let [reservation_request, a_request, aaaa_request] = requests.as_slice() else {
            panic!("expected one reservation and two record requests");
        };
        assert!(
            reservation_request
                .head
                .starts_with("POST /v1/domains HTTP/1.1\r\n")
        );
        assert!(
            !reservation_request
                .head
                .to_ascii_lowercase()
                .contains("authorization:")
        );
        assert!(reservation_request.body.is_empty());
        for request in [a_request, aaaa_request] {
            assert!(
                request
                    .head
                    .starts_with("POST /v1/domains/opaque.ployz.example/records HTTP/1.1\r\n")
            );
            assert!(
                request
                    .head
                    .to_ascii_lowercase()
                    .contains("authorization: bearer raw-token\r\n")
            );
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&a_request.body).unwrap(),
            serde_json::json!({"name":"*","type":"A","values":["192.0.2.1"]})
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&aaaa_request.body).unwrap(),
            serde_json::json!({"name":"*","type":"AAAA","values":["2001:db8::1"]})
        );
    }

    #[tokio::test]
    async fn no_domain_authentication_is_distinct_and_stops_the_sequence() {
        let (endpoint, requests) = fake_server([(
            401,
            r#"{"status":401,"msg":"unauthorized","data":{"noDomain":true}}"#,
        )])
        .await;
        let reservation = super::Reservation {
            endpoint,
            name: "gone.example".into(),
            token: "expired".into(),
        };

        let error = HostedDnsClient::new()
            .create_records(
                &reservation,
                &[DnsRecordRequest {
                    name: "*".into(),
                    record_type: DnsRecordType::A,
                    values: vec!["192.0.2.1".into()],
                }],
            )
            .await
            .unwrap_err();

        assert!(matches!(error, Error::AuthNoDomain));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    async fn fake_server<const N: usize>(
        responses: [(u16, &'static str); N],
    ) -> (String, Arc<Mutex<Vec<Request>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                captured.lock().unwrap().push(request);
                let reason = if status == 200 { "OK" } else { "Unauthorized" };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        (format!("http://{address}/v1"), requests)
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Request {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(chunk.get(..read).unwrap());
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let head = String::from_utf8(bytes.get(..header_end).unwrap().to_vec()).unwrap();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or_default();
        while bytes.len() < header_end + content_length {
            let mut chunk = [0; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(chunk.get(..read).unwrap());
        }
        Request {
            head,
            body: bytes
                .get(header_end..header_end + content_length)
                .unwrap()
                .to_vec(),
        }
    }
}
