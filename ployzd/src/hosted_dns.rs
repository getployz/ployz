use bytes::Bytes;
use ployz_core::{DnsRecord, IngressHost};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::corrosion::ReplicatedStore;

// Hosted DNS still uses Uncloud's API at dns.uncloud.run. Point this at a
// Ployz endpoint when that service exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ReservationWire")]
pub(crate) struct Reservation {
    endpoint: String,
    name: IngressHost,
    // TODO: encrypt the token in the store.
    token: ReservationToken,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
struct ReservationToken(String);

#[derive(Deserialize)]
struct ReservationWire {
    endpoint: String,
    name: String,
    token: String,
}

impl Reservation {
    pub(crate) fn new(endpoint: String, name: String, token: String) -> Result<Self, Error> {
        endpoint_url(&endpoint, &[])?;
        let name = IngressHost::parse(name.strip_suffix('.').unwrap_or(&name).to_ascii_lowercase())
            .map_err(|_| Error::InvalidReservation("invalid DNS hostname"))?;
        if token.is_empty() || http::HeaderValue::from_str(&format!("Bearer {token}")).is_err() {
            return Err(Error::InvalidReservation("invalid reservation token"));
        }
        Ok(Self {
            endpoint,
            name,
            token: ReservationToken(token),
        })
    }

    pub(crate) fn name(&self) -> &str {
        self.name.as_str()
    }
}

impl TryFrom<ReservationWire> for Reservation {
    type Error = Error;

    fn try_from(wire: ReservationWire) -> Result<Self, Self::Error> {
        Self::new(wire.endpoint, wire.name, wire.token)
    }
}

#[derive(Clone)]
pub(crate) struct HostedDns {
    client: Client,
}

impl HostedDns {
    pub(crate) fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub(crate) async fn reserve_domain(
        &self,
        store: &ReplicatedStore,
        endpoint: &str,
    ) -> Result<String, Error> {
        if store.domain_reservation().await?.is_some() {
            return Err(Error::AlreadyReserved);
        }
        let reservation = self.request_reservation(endpoint).await?;
        let name = reservation.name().to_owned();
        store.publish_domain_reservation(&reservation).await?;
        Ok(name)
    }

    pub(crate) async fn domain(&self, store: &ReplicatedStore) -> Result<String, Error> {
        store
            .domain_reservation()
            .await?
            .map(|reservation| reservation.name().to_owned())
            .ok_or(Error::NotFound)
    }

    pub(crate) async fn release_domain(&self, store: &ReplicatedStore) -> Result<String, Error> {
        let reservation = match store.domain_reservation().await {
            Ok(reservation) => reservation.ok_or(Error::NotFound)?,
            Err(crate::corrosion::Error::InvalidDomainReservation(_)) => {
                store.remove_domain_reservation().await?;
                return Err(Error::InvalidReservationCleared);
            }
            Err(error) => return Err(error.into()),
        };
        // ponytail: purge is best-effort. Hosted PersistRecord leaves stale
        // values after upsert, so purgerecords 500s; age-purge removes leftovers.
        let _ = self.purge_hosted_records(&reservation).await;
        store.remove_domain_reservation().await?;
        Ok(reservation.name().to_owned())
    }

    pub(crate) async fn create_records(
        &self,
        store: &ReplicatedStore,
        records: &[DnsRecord],
    ) -> Result<Vec<DnsRecord>, Error> {
        let reservation = store.domain_reservation().await?.ok_or(Error::NotFound)?;
        self.submit_records(&reservation, records).await
    }

    async fn request_reservation(&self, endpoint: &str) -> Result<Reservation, Error> {
        let response = self
            .client
            .post(endpoint_url(endpoint, &["domains"])?)
            .send()
            .await?;
        let response: DomainResponse = decode(response).await?;
        Reservation::new(endpoint.to_owned(), response.name, response.token)
    }

    async fn purge_hosted_records(&self, reservation: &Reservation) -> Result<(), Error> {
        let response = self
            .client
            .post(endpoint_url(
                &reservation.endpoint,
                &["domains", reservation.name(), "purgerecords"],
            )?)
            .bearer_auth(&reservation.token.0)
            .send()
            .await?;
        match hosted_body(response).await {
            Ok(_) | Err(Error::AuthNoDomain) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn submit_records(
        &self,
        reservation: &Reservation,
        records: &[DnsRecord],
    ) -> Result<Vec<DnsRecord>, Error> {
        let url = endpoint_url(
            &reservation.endpoint,
            &["domains", reservation.name(), "records"],
        )?;
        let mut created = Vec::with_capacity(records.len());
        for record in records {
            let response = self
                .client
                .post(url.clone())
                .bearer_auth(&reservation.token.0)
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
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.port() == Some(0)
    {
        return Err(Error::InvalidEndpoint(
            "expected an HTTP(S) URL with a host and nonzero port".into(),
        ));
    }
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
    Ok(serde_json::from_slice(&hosted_body(response).await?)?)
}

async fn hosted_body(response: reqwest::Response) -> Result<Bytes, Error> {
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
        return Err(Error::Status(
            status.as_u16(),
            String::from_utf8_lossy(&body).into_owned(),
        ));
    }
    Ok(body)
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

#[derive(Default, Deserialize)]
struct AuthError {
    #[serde(default)]
    data: AuthErrorData,
}

#[derive(Default, Deserialize)]
struct AuthErrorData {
    #[serde(default, rename = "noDomain")]
    no_domain: bool,
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("Cluster domain is already reserved")]
    AlreadyReserved,
    #[error("Cluster domain was not found")]
    NotFound,
    #[error("replicated hosted DNS state failed: {0}")]
    Store(#[from] crate::corrosion::Error),
    #[error("hosted DNS request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("hosted DNS response was invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid hosted DNS endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("invalid hosted DNS reservation: {0}")]
    InvalidReservation(&'static str),
    #[error(
        "invalid hosted DNS reservation cleared locally; remote record purge was not attempted"
    )]
    InvalidReservationCleared,
    #[error("hosted DNS returned HTTP {0}: {1}")]
    Status(u16, String),
    #[error("hosted DNS authentication failed")]
    Authentication,
    #[error("the supplied domain failed authentication")]
    AuthNoDomain,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ployz_core::{DnsRecord, DnsRecordType};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::{Error, HostedDns};

    #[derive(Debug)]
    struct Request {
        head: String,
        body: Vec<u8>,
    }

    #[test]
    fn persisted_reservation_admits_dns_names_and_rejects_unusable_fields() {
        let valid = serde_json::json!({"endpoint": "https://dns.example/v1", "name": "9-Cluster.Example.", "token": "opaque+/=token"});
        let reservation: super::Reservation = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(reservation.name(), "9-cluster.example");
        let encoded = serde_json::to_value(&reservation).unwrap();
        assert_eq!(
            serde_json::from_value::<super::Reservation>(encoded).unwrap(),
            reservation
        );
        for (field, value) in [
            ("name", ""),
            ("name", "a..example"),
            ("name", "-a.example"),
            ("name", "*.example"),
            ("name", "a/example"),
            ("token", ""),
            ("token", "bad\r\ntoken"),
            ("endpoint", "garbage"),
            ("endpoint", "file:///tmp/dns"),
            ("endpoint", "https://dns.example:0"),
        ] {
            let mut invalid = valid.clone();
            *invalid.get_mut(field).unwrap() = value.into();
            assert!(
                serde_json::from_value::<super::Reservation>(invalid).is_err(),
                "{field}: {value:?}"
            );
        }
    }

    #[tokio::test]
    async fn successful_reservation_response_rejects_unusable_fields() {
        for body in [
            r#"{"name":"","token":"raw-token"}"#,
            r#"{"name":"bad/name.example","token":"raw-token"}"#,
            r#"{"name":"cluster.example","token":""}"#,
        ] {
            let (endpoint, _) = fake_server([(200, body)]).await;
            assert!(
                HostedDns::new()
                    .request_reservation(&endpoint)
                    .await
                    .is_err(),
                "{body}"
            );
        }
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
                r#"{"name":"*","type":"A","values":["203.0.113.9"],"fqdn":"*.opaque.ployz.example"}"#,
            ),
            (
                200,
                r#"{"name":"*","type":"AAAA","values":["2001:db8::99"],"fqdn":"*.opaque.ployz.example"}"#,
            ),
        ];
        let (endpoint, requests) = fake_server(responses).await;
        let client = HostedDns::new();

        let reservation = client.request_reservation(&endpoint).await.unwrap();
        assert_eq!(reservation.name(), "opaque.ployz.example");
        assert_eq!(reservation.token.0, "raw-token");
        let records = client
            .submit_records(
                &reservation,
                &[
                    DnsRecord {
                        name: "*".into(),
                        record_type: DnsRecordType::A,
                        values: vec!["192.0.2.1".into()],
                    },
                    DnsRecord {
                        name: "*".into(),
                        record_type: DnsRecordType::Aaaa,
                        values: vec!["2001:db8::1".into()],
                    },
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            records,
            vec![
                DnsRecord {
                    name: "*.opaque.ployz.example".into(),
                    record_type: DnsRecordType::A,
                    values: vec!["203.0.113.9".into()],
                },
                DnsRecord {
                    name: "*.opaque.ployz.example".into(),
                    record_type: DnsRecordType::Aaaa,
                    values: vec!["2001:db8::99".into()],
                },
            ]
        );

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
    async fn release_purges_hosted_records_even_when_the_domain_has_none() {
        let (endpoint, requests) = fake_server([(202, r#"{"name":"opaque.ployz.example"}"#)]).await;
        let reservation =
            super::Reservation::new(endpoint, "opaque.ployz.example".into(), "raw-token".into())
                .unwrap();

        HostedDns::new()
            .purge_hosted_records(&reservation)
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let requests = requests.lock().unwrap();
        let [purge] = requests.as_slice() else {
            panic!("expected one purge request");
        };
        assert!(
            purge
                .head
                .starts_with("POST /v1/domains/opaque.ployz.example/purgerecords HTTP/1.1\r\n")
        );
        assert!(
            purge
                .head
                .to_ascii_lowercase()
                .contains("authorization: bearer raw-token\r\n")
        );
        assert!(purge.body.is_empty());
    }

    #[tokio::test]
    async fn release_succeeds_when_the_hosted_domain_is_already_gone() {
        let (endpoint, requests) = fake_server([(
            401,
            r#"{"status":401,"msg":"unauthorized","data":{"noDomain":true}}"#,
        )])
        .await;
        let reservation =
            super::Reservation::new(endpoint, "gone.example".into(), "expired".into()).unwrap();

        HostedDns::new()
            .purge_hosted_records(&reservation)
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn release_keeps_a_generic_authentication_failure() {
        let (endpoint, _) = fake_server([(401, r#"{"status":401}"#)]).await;
        let reservation =
            super::Reservation::new(endpoint, "opaque.ployz.example".into(), "wrong".into())
                .unwrap();

        let error = HostedDns::new()
            .purge_hosted_records(&reservation)
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Authentication));
    }

    #[tokio::test]
    async fn release_keeps_a_hosted_purge_failure() {
        let (endpoint, _) = fake_server([(500, r#"{"error":"route53"}"#)]).await;
        let reservation =
            super::Reservation::new(endpoint, "opaque.ployz.example".into(), "raw-token".into())
                .unwrap();

        let error = HostedDns::new()
            .purge_hosted_records(&reservation)
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Status(500, _)));
        let display = error.to_string();
        assert!(display.contains("HTTP 500"), "{display}");
        assert!(display.contains("route53"), "{display}");
        assert_ne!(display, "hosted DNS returned HTTP 500");
    }

    #[tokio::test]
    async fn no_domain_authentication_is_distinct_and_stops_the_sequence() {
        let (endpoint, requests) = fake_server([(
            401,
            r#"{"status":401,"msg":"unauthorized","data":{"noDomain":true}}"#,
        )])
        .await;
        let reservation =
            super::Reservation::new(endpoint, "gone.example".into(), "expired".into()).unwrap();

        let error = HostedDns::new()
            .submit_records(
                &reservation,
                &[DnsRecord {
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

    #[tokio::test]
    async fn other_valid_unauthorized_bodies_are_generic_authentication_failures() {
        let (endpoint, _) = fake_server([(401, r#"{"status":401}"#)]).await;

        let error = HostedDns::new()
            .request_reservation(&endpoint)
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Authentication));
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
