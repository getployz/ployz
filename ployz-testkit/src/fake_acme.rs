//! In-process ACME directory for tests. No real certificate authority.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, CertifiedIssuer, DnType,
    IsCa, KeyPair, KeyUsagePurpose,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

/// Fake ACME server. Directory URLs and HTTP-01 validation are configurable.
#[derive(Clone)]
pub struct FakeCa {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    next_id: u64,
    nonces: HashSet<String>,
    accounts: HashMap<String, String>,
    orders: HashMap<String, Order>,
    certs: HashMap<String, String>,
    ordered: Vec<String>,
    issuer: CertifiedIssuer<'static, KeyPair>,
    base: String,
    port: u16,
    validation_host: String,
    validation_port: u16,
    certificate_lifetime: Option<Duration>,
}

struct Order {
    hostname: String,
    status: OrderStatus,
    token: String,
    thumbprint: String,
}

enum OrderStatus {
    Pending,
    Ready,
    Valid { url: String },
    Invalid,
}

impl OrderStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Valid { .. } => "valid",
            Self::Invalid => "invalid",
        }
    }

    fn authorization_status(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready | Self::Valid { .. } => "valid",
            Self::Invalid => "invalid",
        }
    }
}

impl FakeCa {
    /// Bind an ACME directory. Unspecified bind addresses advertise `127.0.0.1`.
    pub async fn bind(addr: &str) -> io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        let port = local.port();
        let host = advertised_host(&local);
        let base = format!("http://{host}:{port}");
        let ca = Self {
            inner: Arc::new(Mutex::new(Inner {
                next_id: 0,
                nonces: HashSet::new(),
                accounts: HashMap::new(),
                orders: HashMap::new(),
                certs: HashMap::new(),
                ordered: Vec::new(),
                issuer: fake_issuer(),
                base,
                port,
                validation_host: "127.0.0.1".into(),
                validation_port: 0,
                certificate_lifetime: None,
            })),
        };
        let app = Router::new()
            .route("/directory", get(directory))
            .route("/new-nonce", get(new_nonce).head(new_nonce))
            .route("/new-account", post(new_account))
            .route("/new-order", post(new_order))
            .route("/order/{id}", post(order))
            .route("/authz/{id}", post(authz))
            .route("/chall/{id}", post(chall))
            .route("/finalize/{id}", post(finalize))
            .route("/cert/{id}", post(cert))
            .with_state(ca.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fake CA server");
        });
        Ok(ca)
    }

    #[must_use]
    pub fn directory_url(&self) -> String {
        format!(
            "{}/directory",
            self.inner.lock().expect("fake CA lock").base
        )
    }

    pub fn set_advertised_host(&self, host: &str) {
        let mut inner = self.inner.lock().expect("fake CA lock");
        inner.base = format!("http://{host}:{}", inner.port);
    }

    pub fn set_validation(&self, host: impl Into<String>, port: u16) {
        let mut inner = self.inner.lock().expect("fake CA lock");
        inner.validation_host = host.into();
        inner.validation_port = port;
    }

    pub fn set_certificate_lifetime(&self, lifetime: Duration) {
        self.inner
            .lock()
            .expect("fake CA lock")
            .certificate_lifetime = Some(lifetime);
    }

    #[must_use]
    pub fn ordered(&self) -> Vec<String> {
        self.inner.lock().expect("fake CA lock").ordered.clone()
    }
}

/// Answer HTTP-01 on a local port for in-process issuance tests.
#[must_use]
pub fn serve_http01(answers: Arc<Mutex<BTreeMap<String, String>>>) -> (oneshot::Sender<()>, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("HTTP-01 bind");
    listener.set_nonblocking(true).expect("HTTP-01 nonblocking");
    let port = listener.local_addr().expect("HTTP-01 port").port();
    let (stop, stopped) = oneshot::channel();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).expect("HTTP-01 listener");
        tokio::select! {
            () = async {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let answers = Arc::clone(&answers);
                    tokio::spawn(async move {
                        let _ = respond_http01(&mut stream, &answers).await;
                    });
                }
            } => {}
            _ = stopped => {}
        }
    });
    (stop, port)
}

fn order_body(base: &str, id: &str, rec: &Order) -> Value {
    let mut body = json!({
        "status": rec.status.as_str(),
        "identifiers": [{"type":"dns","value": rec.hostname}],
        "authorizations": [format!("{base}/authz/{id}")],
        "finalize": format!("{base}/finalize/{id}"),
    });
    if let OrderStatus::Valid { url } = &rec.status
        && let Some(object) = body.as_object_mut()
    {
        object.insert("certificate".into(), json!(url));
    }
    body
}

fn advertised_host(addr: &std::net::SocketAddr) -> String {
    match addr {
        std::net::SocketAddr::V4(v4) if v4.ip().is_unspecified() => "127.0.0.1".into(),
        std::net::SocketAddr::V6(v6) if v6.ip().is_unspecified() => "127.0.0.1".into(),
        std::net::SocketAddr::V4(v4) => v4.ip().to_string(),
        std::net::SocketAddr::V6(v6) => format!("[{}]", v6.ip()),
    }
}

async fn respond_http01(
    stream: &mut tokio::net::TcpStream,
    answers: &Mutex<BTreeMap<String, String>>,
) -> io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0; 2048];
    let n = stream.read(&mut buf).await?;
    let head = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
    let token = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| path.strip_prefix("/.well-known/acme-challenge/"))
        .unwrap_or_default();
    let body = answers
        .lock()
        .expect("HTTP-01 answers")
        .get(token)
        .cloned()
        .unwrap_or_default();
    let status = if body.is_empty() {
        "404 Not Found"
    } else {
        "200 OK"
    };
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
}

async fn directory(State(ca): State<FakeCa>) -> impl IntoResponse {
    let base = ca.inner.lock().expect("fake CA lock").base.clone();
    with_nonce(
        &ca,
        StatusCode::OK,
        None,
        json!({
            "newNonce": format!("{base}/new-nonce"),
            "newAccount": format!("{base}/new-account"),
            "newOrder": format!("{base}/new-order"),
        }),
    )
}

async fn new_nonce(State(ca): State<FakeCa>) -> impl IntoResponse {
    with_nonce(&ca, StatusCode::OK, None, Value::Null)
}

async fn new_account(State(ca): State<FakeCa>, request: Request<Body>) -> Response {
    let Some((protected, _)) = decode_jws(&ca, request).await else {
        return bad_nonce(&ca);
    };
    let thumbprint = protected
        .jwk
        .as_ref()
        .map(jwk_thumbprint)
        .unwrap_or_default();
    let mut inner = ca.inner.lock().expect("fake CA lock");
    inner.next_id += 1;
    let kid = format!("{}/acct/{}", inner.base, inner.next_id);
    inner.accounts.insert(kid.clone(), thumbprint);
    drop(inner);
    with_nonce(
        &ca,
        StatusCode::CREATED,
        Some(kid),
        json!({"status":"valid"}),
    )
}

async fn new_order(State(ca): State<FakeCa>, request: Request<Body>) -> Response {
    let Some((protected, payload)) = decode_jws(&ca, request).await else {
        return bad_nonce(&ca);
    };
    let hostname = payload
        .get("identifiers")
        .and_then(Value::as_array)
        .and_then(|identifiers| identifiers.first())
        .and_then(|identifier| identifier.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let thumbprint = {
        let inner = ca.inner.lock().expect("fake CA lock");
        protected
            .kid
            .as_ref()
            .and_then(|kid| inner.accounts.get(kid).cloned())
            .unwrap_or_default()
    };
    let mut inner = ca.inner.lock().expect("fake CA lock");
    inner.next_id += 1;
    let id = inner.next_id.to_string();
    let location = format!("{}/order/{id}", inner.base);
    inner.ordered.push(hostname.clone());
    inner.orders.insert(
        id.clone(),
        Order {
            hostname,
            status: OrderStatus::Pending,
            token: format!("token{id}"),
            thumbprint,
        },
    );
    let rec = inner.orders.get(&id).expect("just inserted");
    let body = order_body(&inner.base, &id, rec);
    drop(inner);
    with_nonce(&ca, StatusCode::CREATED, Some(location), body)
}

async fn order(
    State(ca): State<FakeCa>,
    Path(id): Path<String>,
    request: Request<Body>,
) -> Response {
    if decode_jws(&ca, request).await.is_none() {
        return bad_nonce(&ca);
    }
    let inner = ca.inner.lock().expect("fake CA lock");
    let Some(rec) = inner.orders.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let body = order_body(&inner.base, &id, rec);
    drop(inner);
    with_nonce(&ca, StatusCode::OK, None, body)
}

async fn authz(
    State(ca): State<FakeCa>,
    Path(id): Path<String>,
    request: Request<Body>,
) -> Response {
    if decode_jws(&ca, request).await.is_none() {
        return bad_nonce(&ca);
    }
    let inner = ca.inner.lock().expect("fake CA lock");
    let Some(rec) = inner.orders.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let body = json!({
        "status": rec.status.authorization_status(),
        "identifier": {"type":"dns","value": rec.hostname},
        "challenges": [{
            "type": "http-01",
            "url": format!("{}/chall/{id}", inner.base),
            "status": rec.status.authorization_status(),
            "token": rec.token,
        }],
    });
    drop(inner);
    with_nonce(&ca, StatusCode::OK, None, body)
}

async fn chall(
    State(ca): State<FakeCa>,
    Path(id): Path<String>,
    request: Request<Body>,
) -> Response {
    if decode_jws(&ca, request).await.is_none() {
        return bad_nonce(&ca);
    }
    let (token, thumbprint, hostname, host, port) = {
        let inner = ca.inner.lock().expect("fake CA lock");
        let Some(rec) = inner.orders.get(&id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        (
            rec.token.clone(),
            rec.thumbprint.clone(),
            rec.hostname.clone(),
            inner.validation_host.clone(),
            inner.validation_port,
        )
    };
    let expected = format!("{token}.{thumbprint}");
    let url = format!("http://{host}:{port}/.well-known/acme-challenge/{token}");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .expect("HTTP-01 client");
    let body = client
        .get(url)
        .header(reqwest::header::HOST, hostname)
        .send()
        .await
        .ok()
        .and_then(|response| response.status().is_success().then_some(response));
    let got = match body {
        Some(response) => response.text().await.unwrap_or_default(),
        None => String::new(),
    };
    let mut inner = ca.inner.lock().expect("fake CA lock");
    let base = inner.base.clone();
    let Some(rec) = inner.orders.get_mut(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    rec.status = if got == expected {
        OrderStatus::Ready
    } else {
        OrderStatus::Invalid
    };
    let body = json!({
        "type": "http-01",
        "url": format!("{base}/chall/{id}"),
        "status": rec.status.authorization_status(),
        "token": rec.token,
    });
    drop(inner);
    with_nonce(&ca, StatusCode::OK, None, body)
}

async fn finalize(
    State(ca): State<FakeCa>,
    Path(id): Path<String>,
    request: Request<Body>,
) -> Response {
    let Some((_, payload)) = decode_jws(&ca, request).await else {
        return bad_nonce(&ca);
    };
    let csr = payload
        .get("csr")
        .and_then(Value::as_str)
        .and_then(|csr| URL_SAFE_NO_PAD.decode(csr).ok())
        .unwrap_or_default();
    let mut inner = ca.inner.lock().expect("fake CA lock");
    let Some(order) = inner.orders.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !matches!(order.status, OrderStatus::Ready) {
        let body = order_body(&inner.base, &id, order);
        drop(inner);
        return with_nonce(&ca, StatusCode::OK, None, body);
    }
    let issued = sign_csr(&inner.issuer, &csr, inner.certificate_lifetime);
    inner.next_id += 1;
    let cert_url = format!("{}/cert/{}", inner.base, inner.next_id);
    let cert_id = inner.next_id.to_string();
    inner.certs.insert(cert_id, issued);
    let order = inner.orders.get_mut(&id).expect("order still present");
    order.status = OrderStatus::Valid { url: cert_url };
    let rec = inner.orders.get(&id).expect("order still present");
    let body = order_body(&inner.base, &id, rec);
    drop(inner);
    with_nonce(&ca, StatusCode::OK, None, body)
}

async fn cert(
    State(ca): State<FakeCa>,
    Path(id): Path<String>,
    request: Request<Body>,
) -> Response {
    if decode_jws(&ca, request).await.is_none() {
        return bad_nonce(&ca);
    }
    let pem = ca
        .inner
        .lock()
        .expect("fake CA lock")
        .certs
        .get(&id)
        .cloned();
    let Some(pem) = pem else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let nonce = next_nonce(&ca);
    (
        StatusCode::OK,
        [
            ("Replay-Nonce", nonce),
            ("Cache-Control", "no-store".into()),
            ("Content-Type", "application/pem-certificate-chain".into()),
        ],
        pem,
    )
        .into_response()
}

async fn decode_jws(ca: &FakeCa, request: Request<Body>) -> Option<(Protected, Value)> {
    let body = axum::body::to_bytes(request.into_body(), 1_000_000)
        .await
        .ok()?;
    let jws: Jws = serde_json::from_slice(&body).ok()?;
    let protected_bytes = b64url(&jws.protected)?;
    let protected: Protected = serde_json::from_slice(&protected_bytes).ok()?;
    let nonce = protected.nonce.clone()?;
    {
        let mut inner = ca.inner.lock().expect("fake CA lock");
        if !inner.nonces.remove(&nonce) {
            return None;
        }
    }
    let payload = if jws.payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&b64url(&jws.payload)?).ok()?
    };
    Some((protected, payload))
}

fn bad_nonce(ca: &FakeCa) -> Response {
    with_nonce(
        ca,
        StatusCode::BAD_REQUEST,
        None,
        json!({"type":"urn:ietf:params:acme:error:badNonce","detail":"bad nonce"}),
    )
}

fn with_nonce(ca: &FakeCa, status: StatusCode, location: Option<String>, body: Value) -> Response {
    let nonce = next_nonce(ca);
    let mut response = if body.is_null() {
        status.into_response()
    } else {
        (
            status,
            [("Content-Type", "application/json")],
            body.to_string(),
        )
            .into_response()
    };
    response
        .headers_mut()
        .insert("Replay-Nonce", nonce.parse().expect("nonce header"));
    response
        .headers_mut()
        .insert("Cache-Control", "no-store".parse().expect("cache header"));
    if let Some(location) = location {
        response
            .headers_mut()
            .insert("Location", location.parse().expect("location header"));
    }
    response
}

fn next_nonce(ca: &FakeCa) -> String {
    let mut inner = ca.inner.lock().expect("fake CA lock");
    inner.next_id += 1;
    let nonce = format!("n{}", inner.next_id);
    inner.nonces.insert(nonce.clone());
    nonce
}

fn fake_issuer() -> CertifiedIssuer<'static, KeyPair> {
    let key = KeyPair::generate().expect("fake CA key");
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "Ployz fake CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    CertifiedIssuer::self_signed(params, key).expect("fake CA issuer")
}

fn sign_csr(
    issuer: &CertifiedIssuer<'static, KeyPair>,
    csr: &[u8],
    lifetime: Option<Duration>,
) -> String {
    let mut csr = CertificateSigningRequestParams::from_der(&csr.to_vec().into()).expect("CSR DER");
    if let Some(lifetime) = lifetime {
        let now = time::OffsetDateTime::now_utc();
        csr.params.not_before = now;
        csr.params.not_after =
            now + time::Duration::try_from(lifetime).unwrap_or(time::Duration::MAX);
    }
    let cert = csr.signed_by(issuer).expect("sign CSR");
    format!("{}{}", cert.pem(), issuer.pem())
}

/// Self-signed certificate and key with an exact validity window.
#[must_use]
pub fn self_signed_material(
    hostname: &str,
    not_before: SystemTime,
    not_after: SystemTime,
) -> (String, String) {
    let key = KeyPair::generate().expect("test key");
    let mut params = CertificateParams::new(vec![hostname.to_owned()]).expect("test params");
    params.not_before = time::OffsetDateTime::from(not_before);
    params.not_after = time::OffsetDateTime::from(not_after);
    let cert = params.self_signed(&key).expect("test certificate");
    (cert.pem(), key.serialize_pem())
}

fn jwk_thumbprint(jwk: &Value) -> String {
    let canonical = format!(
        r#"{{"crv":"{}","kty":"EC","x":"{}","y":"{}"}}"#,
        jwk["crv"].as_str().unwrap_or_default(),
        jwk["x"].as_str().unwrap_or_default(),
        jwk["y"].as_str().unwrap_or_default(),
    );
    URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()))
}

fn b64url(input: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(input).ok()
}

#[derive(Deserialize)]
struct Jws {
    protected: String,
    payload: String,
}

#[derive(Deserialize)]
struct Protected {
    nonce: Option<String>,
    jwk: Option<Value>,
    kid: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use base64::Engine;
    use serde_json::{Value, json};

    use super::{
        CertificateParams, FakeCa, KeyPair, URL_SAFE_NO_PAD, jwk_thumbprint, serve_http01,
    };

    struct Acme {
        http: reqwest::Client,
        nonce: String,
        kid: Option<String>,
        directory: Value,
    }

    impl Acme {
        async fn open(directory_url: &str) -> Self {
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap();
            let response = http.get(directory_url).send().await.unwrap();
            let nonce = header(&response, "replay-nonce");
            let directory = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            Self {
                http,
                nonce,
                kid: None,
                directory,
            }
        }

        async fn post(&mut self, url: &str, payload: Value) -> (Option<String>, Value) {
            let mut protected = serde_json::Map::from_iter([
                ("alg".into(), json!("ES256")),
                ("nonce".into(), json!(self.nonce)),
                ("url".into(), json!(url)),
            ]);
            match &self.kid {
                Some(kid) => protected.insert("kid".into(), json!(kid)),
                None => protected.insert("jwk".into(), jwk()),
            };
            let payload = if payload.is_null() {
                String::new()
            } else {
                URL_SAFE_NO_PAD.encode(payload.to_string())
            };
            let response = self
                .http
                .post(url)
                .header("content-type", "application/json")
                .body(
                    json!({
                        "protected": URL_SAFE_NO_PAD.encode(Value::Object(protected).to_string()),
                        "payload": payload,
                        "signature": "e30",
                    })
                    .to_string(),
                )
                .send()
                .await
                .unwrap();
            self.nonce = header(&response, "replay-nonce");
            let location = response
                .headers()
                .get("location")
                .map(|value| value.to_str().unwrap().to_owned());
            let body = response.bytes().await.unwrap();
            (
                location,
                serde_json::from_slice(&body).unwrap_or(Value::Null),
            )
        }

        async fn register_and_order(&mut self, hostname: &str) -> (String, Value) {
            let new_account = json_str(&self.directory, "/newAccount").to_owned();
            let new_order = json_str(&self.directory, "/newOrder").to_owned();
            let (kid, _) = self.post(&new_account, json!({})).await;
            self.kid = Some(kid.unwrap());
            let (location, body) = self
                .post(
                    &new_order,
                    json!({"identifiers":[{"type":"dns","value": hostname}]}),
                )
                .await;
            (location.unwrap(), body)
        }
    }

    fn jwk() -> Value {
        json!({"crv":"P-256","kty":"EC","x":"x","y":"y"})
    }

    fn json_str<'a>(value: &'a Value, pointer: &str) -> &'a str {
        value.pointer(pointer).and_then(Value::as_str).unwrap()
    }

    fn header(response: &reqwest::Response, name: &str) -> String {
        response
            .headers()
            .get(name)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }

    fn csr(hostname: &str) -> String {
        let key = KeyPair::generate().unwrap();
        let params = CertificateParams::new(vec![hostname.to_owned()]).unwrap();
        URL_SAFE_NO_PAD.encode(params.serialize_request(&key).unwrap().der())
    }

    #[tokio::test]
    async fn failed_http01_sets_the_order_invalid() {
        let ca = FakeCa::bind("127.0.0.1:0").await.unwrap();
        let mut acme = Acme::open(&ca.directory_url()).await;
        let (order_url, order) = acme.register_and_order("app.example.com").await;
        let authz_url = json_str(&order, "/authorizations/0").to_owned();
        let (_, authz) = acme.post(&authz_url, Value::Null).await;
        let chall_url = json_str(&authz, "/challenges/0/url").to_owned();

        acme.post(&chall_url, Value::Null).await;
        let (_, order) = acme.post(&order_url, Value::Null).await;
        let (_, authz) = acme.post(&authz_url, Value::Null).await;

        assert_eq!(json_str(&order, "/status"), "invalid");
        assert!(order.get("certificate").is_none());
        assert_eq!(json_str(&authz, "/status"), "invalid");
        assert_eq!(json_str(&authz, "/challenges/0/status"), "invalid");
    }

    #[tokio::test]
    async fn successful_challenge_reaches_ready_then_valid_with_a_url() {
        let answers = Arc::new(Mutex::new(BTreeMap::new()));
        let (http01, port) = serve_http01(Arc::clone(&answers));
        let ca = FakeCa::bind("127.0.0.1:0").await.unwrap();
        ca.set_validation("127.0.0.1", port);
        let mut acme = Acme::open(&ca.directory_url()).await;
        let (order_url, order) = acme.register_and_order("app.example.com").await;
        let (_, authz) = acme
            .post(json_str(&order, "/authorizations/0"), Value::Null)
            .await;
        let token = json_str(&authz, "/challenges/0/token");
        let chall_url = json_str(&authz, "/challenges/0/url").to_owned();
        let key_authorization = format!("{token}.{}", jwk_thumbprint(&jwk()));
        answers
            .lock()
            .unwrap()
            .insert(token.to_owned(), key_authorization);

        acme.post(&chall_url, Value::Null).await;
        let (_, order) = acme.post(&order_url, Value::Null).await;
        assert_eq!(json_str(&order, "/status"), "ready");
        assert!(order.get("certificate").is_none());

        acme.post(
            json_str(&order, "/finalize"),
            json!({"csr": csr("app.example.com")}),
        )
        .await;
        let (_, order) = acme.post(&order_url, Value::Null).await;
        assert_eq!(json_str(&order, "/status"), "valid");
        assert!(json_str(&order, "/certificate").contains("/cert/"));
        drop(http01);
    }
}
