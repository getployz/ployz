//! Fake Cloud enrollment HTTP and shared orchestration event recording.

use std::{
    collections::VecDeque,
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
    callback_status: Arc<AtomicU16>,
    _server: tokio::task::JoinHandle<()>,
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

    /// First POST claims the name. A later POST with a different public key
    /// returns `not_yet`.
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
        let callback_status = Arc::new(AtomicU16::new(200));
        let recorded_paths = Arc::clone(&paths);
        let recorded_posts = Arc::clone(&posts);
        let recorded_callbacks = Arc::clone(&callbacks);
        let callback_code = Arc::clone(&callback_status);
        let replies = Arc::new(Mutex::new(replies));
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0; 8192];
                let n = stream.read(&mut buf).await.unwrap();
                let raw = buf.get(..n).expect("read count is in bounds");
                let request = String::from_utf8_lossy(raw);
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                recorded_paths.lock().unwrap().push(path.clone());
                if path.ends_with("/callback") {
                    events.record("callback");
                    recorded_callbacks
                        .lock()
                        .unwrap()
                        .push(enroll_json_body(raw));
                    let status = callback_code.load(Ordering::SeqCst);
                    let reason = if status == 200 { "OK" } else { "Error" };
                    write_http(&mut stream, status, reason, &[]).await;
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
            callback_status,
            _server: server,
        }
    }

    pub fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }

    pub fn posts(&self) -> Vec<serde_json::Value> {
        self.posts.lock().unwrap().clone()
    }

    pub fn callbacks(&self) -> Vec<serde_json::Value> {
        self.callbacks.lock().unwrap().clone()
    }

    pub fn set_callback_status(&self, status: u16) {
        self.callback_status.store(status, Ordering::SeqCst);
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
    let posted = post["publicKey"]
        .as_str()
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
    body["registration"]["assigned_machine"]["public_key"] = serde_json::to_value(key).unwrap();
    serde_json::to_vec(&body).unwrap()
}

async fn write_http(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
}

fn enroll_json_body(raw: &[u8]) -> serde_json::Value {
    let sep = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP request has a header separator");
    serde_json::from_slice(raw.get(sep + 4..).expect("body follows headers")).unwrap()
}
