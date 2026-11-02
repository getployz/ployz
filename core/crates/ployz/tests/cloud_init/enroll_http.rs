//! Fake Cloud enrollment HTTP and shared orchestration event recording.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
};

use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Default)]
pub struct EventLog(Arc<Mutex<Vec<&'static str>>>);

impl EventLog {
    pub(super) fn record(&self, event: &'static str) {
        self.0.lock().unwrap().push(event);
    }

    pub fn entries(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

enum EnrollReplies {
    Script(VecDeque<Vec<u8>>),
    Occupy {
        join: serde_json::Value,
        claimed: Option<String>,
    },
}

pub struct EnrollListen {
    pub url: String,
    paths: Arc<Mutex<Vec<String>>>,
    posts: Arc<Mutex<Vec<serde_json::Value>>>,
    callbacks: Arc<Mutex<Vec<serde_json::Value>>>,
    callback_reply: Arc<Mutex<CallbackReply>>,
    publications: Arc<Mutex<Vec<serde_json::Value>>>,
    publication_status: Arc<AtomicU16>,
    cli: Arc<Mutex<CliReplies>>,
    reports: Arc<Mutex<Vec<(usize, serde_json::Value)>>>,
    report_reply: Arc<Mutex<ReportReply>>,
    _server: tokio::task::JoinHandle<()>,
}

/// How the fake Cloud answers a setup report.
#[derive(Clone, Copy)]
pub enum ReportReply {
    Status(u16),
    Hang,
}

/// How the fake Cloud answers the founder callback, after recording it.
#[derive(Clone)]
pub enum CallbackReply {
    Echo,
    Status(u16),
    Body(serde_json::Value),
    Drop,
    Hang,
}

/// Replies to signed-in CLI calls by path; the last reply repeats.
#[derive(Default)]
struct CliReplies {
    replies: HashMap<String, VecDeque<serde_json::Value>>,
    calls: Vec<(String, String, serde_json::Value)>,
}

impl EnrollListen {
    pub async fn start(body: serde_json::Value) -> Self {
        Self::script([body]).await
    }

    pub async fn script(bodies: impl IntoIterator<Item = serde_json::Value>) -> Self {
        Self::script_recording(bodies, EventLog::default()).await
    }

    pub async fn script_recording(
        bodies: impl IntoIterator<Item = serde_json::Value>,
        events: EventLog,
    ) -> Self {
        Self::listen(
            EnrollReplies::Script(
                bodies
                    .into_iter()
                    .map(|body| serde_json::to_vec(&body).unwrap())
                    .collect(),
            ),
            events,
        )
        .await
    }

    pub async fn occupying_join(join: serde_json::Value) -> Self {
        Self::listen(
            EnrollReplies::Occupy {
                join,
                claimed: None,
            },
            EventLog::default(),
        )
        .await
    }

    async fn listen(replies: EnrollReplies, events: EventLog) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let posts = Arc::new(Mutex::new(Vec::new()));
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let publication_status = Arc::new(AtomicU16::new(200));
        let publication_code = Arc::clone(&publication_status);
        let publications = Arc::new(Mutex::new(Vec::new()));
        let recorded_publications = Arc::clone(&publications);
        let callback_reply = Arc::new(Mutex::new(CallbackReply::Echo));
        let recorded_paths = Arc::clone(&paths);
        let recorded_posts = Arc::clone(&posts);
        let recorded_callbacks = Arc::clone(&callbacks);
        let reply_callback = Arc::clone(&callback_reply);
        let replies = Arc::new(Mutex::new(replies));
        let cli = Arc::new(Mutex::new(CliReplies::default()));
        let cli_replies = Arc::clone(&cli);
        let reports = Arc::new(Mutex::new(Vec::new()));
        let recorded_reports = Arc::clone(&reports);
        let report_reply = Arc::new(Mutex::new(ReportReply::Status(200)));
        let reply_report = Arc::clone(&report_reply);
        let server = tokio::spawn(async move {
            let (sent, mut requests) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(accept_requests(listener, sent));
            while let Some((mut stream, raw)) = requests.recv().await {
                let raw: &[u8] = &raw;
                let request = String::from_utf8_lossy(raw);
                // Anything else on this port, such as a stray probe, is refused and changes
                // nothing a test observes. It used to take the scripted enroll reply (#3732).
                let Some(path) = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .filter(|path| {
                        matches!(
                            *path,
                            "/api/enroll" | "/api/enroll/callback" | "/api/enroll/report"
                        ) || path.starts_with("/api/cli/")
                    })
                    .map(str::to_owned)
                else {
                    write_http(&mut stream, 404, "Not Found", &[]).await;
                    continue;
                };
                if path.starts_with("/api/enroll")
                    && !request
                        .lines()
                        .any(|line| line == format!("authorization: Bearer {}", super::TOKEN))
                {
                    write_http(&mut stream, 401, "Unauthorized", &[]).await;
                    continue;
                }
                // Reports stay out of `paths`: they follow every outcome.
                if path.ends_with("/report") {
                    let completions = recorded_callbacks.lock().unwrap().len();
                    recorded_reports
                        .lock()
                        .unwrap()
                        .push((completions, enroll_json_body(raw)));
                    let reply = *reply_report.lock().unwrap();
                    match reply {
                        ReportReply::Status(status) => {
                            write_http(&mut stream, status, "Response", &[]).await;
                        }
                        ReportReply::Hang => {
                            tokio::spawn(async move {
                                let _held = stream;
                                std::future::pending::<()>().await;
                            });
                        }
                    }
                    continue;
                }
                recorded_paths.lock().unwrap().push(path.clone());
                if path.starts_with("/api/cli/") {
                    let bearer = request
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("authorization: bearer ")
                                .map(str::to_owned)
                        })
                        .unwrap_or_default();
                    let body = enroll_json_body(raw);
                    let reply = {
                        let mut cli = cli_replies.lock().unwrap();
                        cli.calls.push((path.clone(), bearer, body));
                        cli.replies.get_mut(&path).and_then(|queue| {
                            if queue.len() > 1 {
                                queue.pop_front()
                            } else {
                                queue.front().cloned()
                            }
                        })
                    };
                    match reply {
                        Some(reply) => {
                            write_http(&mut stream, 200, "OK", &serde_json::to_vec(&reply).unwrap())
                                .await
                        }
                        None => write_http(&mut stream, 404, "Not Found", &[]).await,
                    }
                    continue;
                }
                if path.ends_with("/callback") {
                    let body = enroll_json_body(raw);
                    if body.get("stage").and_then(serde_json::Value::as_str) == Some("publish") {
                        events.record("publish");
                        recorded_publications.lock().unwrap().push(body);
                        write_http(
                            &mut stream,
                            publication_code.load(Ordering::SeqCst),
                            "Response",
                            &[],
                        )
                        .await;
                        continue;
                    }
                    events.record("callback");
                    recorded_callbacks.lock().unwrap().push(body.clone());
                    let reply = reply_callback.lock().unwrap().clone();
                    match reply {
                        CallbackReply::Echo => {
                            let echo = serde_json::json!({ "machineId": body.get("machineId") });
                            let echo = serde_json::to_vec(&echo).unwrap();
                            write_http(&mut stream, 200, "OK", &echo).await;
                        }
                        CallbackReply::Status(status) => {
                            write_http(&mut stream, status, "Error", &[]).await;
                        }
                        CallbackReply::Body(value) => {
                            let echo = serde_json::to_vec(&value).unwrap();
                            write_http(&mut stream, 200, "OK", &echo).await;
                        }
                        CallbackReply::Drop => drop(stream),
                        CallbackReply::Hang => {
                            tokio::spawn(async move {
                                let _held = stream;
                                std::future::pending::<()>().await;
                            });
                        }
                    }
                    continue;
                }
                let post = enroll_json_body(raw);
                recorded_posts.lock().unwrap().push(post.clone());
                let body = replies.lock().unwrap().next(&post);
                write_http(&mut stream, 200, "OK", &body).await;
            }
        });
        Self {
            url: format!("http://{address}"),
            paths,
            posts,
            callbacks,
            callback_reply,
            publications,
            publication_status,
            cli,
            reports,
            report_reply,
            _server: server,
        }
    }

    /// Answer the signed-in CLI call at `path` with `bodies` in turn; the last one repeats.
    pub fn reply_cli(&self, path: &str, bodies: impl IntoIterator<Item = serde_json::Value>) {
        self.cli
            .lock()
            .unwrap()
            .replies
            .insert(path.to_owned(), bodies.into_iter().collect());
    }

    /// Signed-in CLI calls as `(path, lowercased bearer, body)`.
    pub fn cli_calls(&self) -> Vec<(String, String, serde_json::Value)> {
        self.cli.lock().unwrap().calls.clone()
    }

    /// Setup reports in arrival order, each with the completion callbacks seen before it.
    pub fn reports(&self) -> Vec<(usize, serde_json::Value)> {
        self.reports.lock().unwrap().clone()
    }

    pub fn set_report_reply(&self, reply: ReportReply) {
        *self.report_reply.lock().unwrap() = reply;
    }

    pub fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }

    pub fn posts(&self) -> Vec<serde_json::Value> {
        self.posts.lock().unwrap().clone()
    }

    pub fn set_publication_status(&self, status: u16) {
        self.publication_status.store(status, Ordering::SeqCst);
    }

    pub fn publications(&self) -> Vec<serde_json::Value> {
        self.publications.lock().unwrap().clone()
    }

    pub fn callbacks(&self) -> Vec<serde_json::Value> {
        self.callbacks.lock().unwrap().clone()
    }

    pub fn set_callback_status(&self, status: u16) {
        self.set_callback_reply(CallbackReply::Status(status));
    }

    pub fn set_callback_reply(&self, reply: CallbackReply) {
        *self.callback_reply.lock().unwrap() = reply;
    }
}

impl EnrollReplies {
    fn next(&mut self, post: &serde_json::Value) -> Vec<u8> {
        match self {
            Self::Script(remaining) => remaining.pop_front().expect("scripted enroll has a body"),
            Self::Occupy { join, claimed } => occupy_join(join, claimed, post),
        }
    }
}

fn occupy_join(
    join: &serde_json::Value,
    claimed: &mut Option<String>,
    post: &serde_json::Value,
) -> Vec<u8> {
    let posted = post
        .get("publicKey")
        .and_then(serde_json::Value::as_str)
        .expect("enroll POST carries publicKey");
    if let Some(existing) = claimed.as_ref()
        && existing != posted
    {
        return serde_json::to_vec(&serde_json::json!({
            "kind": "not_yet",
            "retryAfter": 0,
        }))
        .unwrap();
    }
    *claimed = Some(posted.to_owned());
    let mut body = join.clone();
    let key = STANDARD.decode(posted).expect("enroll publicKey is base64");
    *body
        .pointer_mut("/registration/assigned_machine/public_key")
        .expect("join registration has public_key") = serde_json::to_value(key).unwrap();
    serde_json::to_vec(&body).unwrap()
}

async fn write_http(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // A client that hung up has nothing to read the answer; its outcome is the client's to see.
    if stream.write_all(header.as_bytes()).await.is_ok() {
        let _ = stream.write_all(body).await;
    }
}

/// Reads each connection on its own, so a client that never finishes its request can't hold
/// up the CLI's, and hands over only whole requests, in the order they complete.
async fn accept_requests(
    listener: TcpListener,
    requests: tokio::sync::mpsc::UnboundedSender<(TcpStream, Vec<u8>)>,
) {
    while let Ok((mut stream, _)) = listener.accept().await {
        let requests = requests.clone();
        tokio::spawn(async move {
            if let Some(raw) = read_request(&mut stream).await {
                let _ = requests.send((stream, raw));
            }
        });
    }
}

/// One whole request: the head, then its `Content-Length` body, however the client split
/// them across writes. `None` when the client hangs up first.
async fn read_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut raw = Vec::new();
    let mut chunk = [0; 8192];
    let head = loop {
        if let Some(sep) = header_end(&raw) {
            break sep;
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(chunk.get(..n)?);
    };
    let length = String::from_utf8_lossy(raw.get(..head)?)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while raw.len() < head + 4 + length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(chunk.get(..n)?);
    }
    Some(raw)
}

fn header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

fn enroll_json_body(raw: &[u8]) -> serde_json::Value {
    let sep = header_end(raw).expect("HTTP request has a header separator");
    let body = raw.get(sep + 4..).expect("body follows headers");
    // A GET has no body.
    if body.is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_slice(body).unwrap()
}

/// Connections other than the CLI's leave the fake Cloud as they found it: a stray request
/// is refused without taking the scripted enroll reply, and a client that hangs up or stalls
/// mid-request holds up nothing (#3732).
#[tokio::test]
async fn stray_connections_leave_the_scripted_enroll_for_the_cli() {
    let enroll = EnrollListen::start(serde_json::json!({ "kind": "not_yet" })).await;
    let address = enroll.url.trim_start_matches("http://");
    let exchange = |request: Vec<u8>| async move {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(&request).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8_lossy(&response).into_owned()
    };

    let probe = exchange(format!("GET / HTTP/1.1\r\nhost: {address}\r\n\r\n").into_bytes()).await;
    assert!(probe.starts_with("HTTP/1.1 404"), "{probe}");
    let hung_up = exchange(b"\x16\x03\x01\x02\x00".to_vec()).await;
    assert_eq!(hung_up, "");
    // Stays open, mid-request, while the CLI enrolls.
    let mut held = TcpStream::connect(address).await.unwrap();
    held.write_all(b"GET / HT").await.unwrap();
    let body = br#"{"publicKey":"cli"}"#;
    let mut enrolled = format!(
        "POST /api/enroll HTTP/1.1\r\nhost: {address}\r\nauthorization: Bearer {}\r\ncontent-length: {}\r\n\r\n",
        super::TOKEN,
        body.len()
    )
    .into_bytes();
    enrolled.extend_from_slice(body);
    let enrolled = exchange(enrolled).await;

    assert!(enrolled.starts_with("HTTP/1.1 200"), "{enrolled}");
    assert!(enrolled.ends_with(r#"{"kind":"not_yet"}"#), "{enrolled}");
    assert_eq!(
        enroll.posts(),
        vec![serde_json::json!({ "publicKey": "cli" })]
    );
    assert_eq!(enroll.paths(), ["/api/enroll".to_owned()]);
    drop(held);
}
