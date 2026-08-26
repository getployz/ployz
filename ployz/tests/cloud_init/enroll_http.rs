//! Fake Cloud enrollment HTTP and shared orchestration event recording.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
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
        let mut remaining: VecDeque<Vec<u8>> = bodies
            .into_iter()
            .map(|body| serde_json::to_vec(&body).unwrap())
            .collect();
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
                let is_callback = path.ends_with("/callback");
                recorded_paths.lock().unwrap().push(path);
                if is_callback {
                    events.record("callback");
                    recorded_callbacks
                        .lock()
                        .unwrap()
                        .push(enroll_json_body(raw));
                    let status = callback_code.load(Ordering::SeqCst);
                    let reason = if status == 200 { "OK" } else { "Error" };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                    continue;
                }
                recorded_posts.lock().unwrap().push(enroll_json_body(raw));
                let body = remaining.pop_front().expect("scripted enroll has a body");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
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

fn enroll_json_body(raw: &[u8]) -> serde_json::Value {
    let sep = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP request has a header separator");
    serde_json::from_slice(raw.get(sep + 4..).expect("body follows headers")).unwrap()
}
