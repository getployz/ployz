use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};

use ployz_core::Namespace;
use regex::RegexSet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    salts::{Lookup, Salts},
    ship::RequestOutbox,
};

const POLL: Duration = Duration::from_millis(200);
const READ_CHUNK: usize = 64 * 1024;
const MAX_LINE: usize = 1024 * 1024;
const MAX_PATH: usize = 1024;
const MAX_HOST: usize = 253;
const MAX_METHOD: usize = 32;
const MAX_REQUEST_ID: usize = 256;
const MAX_QUERY_VALUE: usize = 256;
const MAX_HEADER: usize = 512;
const QUERY_KEYS: [&str; 6] = [
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "ref",
];

static PERSONAL: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"[^@\s]+@[^@\s]+\.[^@\s]+",
        r"eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]*\.",
        r"[A-Za-z0-9_-]{32,}",
    ])
    .expect("the personal-value patterns compile")
});

pub(super) fn tail(path: &Path, salts: &Salts, outbox: &RequestOutbox) {
    let mut tail = Tail::new(path.to_path_buf());
    let harvest = Harvest { salts, outbox };
    loop {
        let read = tail
            .poll(|line| harvest.line(line))
            .unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "cannot read the request log");
                0
            });
        harvest.tick();
        if read == 0 {
            std::thread::sleep(POLL);
        }
    }
}

struct Harvest<'a> {
    salts: &'a Salts,
    outbox: &'a RequestOutbox,
}

impl Harvest<'_> {
    fn line(&self, bytes: &[u8]) {
        let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
            tracing::debug!("skipping an access log line that does not decode");
            return;
        };
        let Ok(namespace) = Namespace::parse(&line.namespace) else {
            return;
        };
        let ts = line.millis();
        let ua = first(line.request.headers.user_agent.as_ref()).unwrap_or_default();
        let visitor = match self.salts.lookup(&namespace, ts.div_euclid(1000)) {
            Lookup::Covered { salt, .. } => {
                Some(visitor(&salt, &namespace, &line.request.client_ip, ua))
            }
            Lookup::Off | Lookup::Gap => None,
        };
        if let Err(error) = self.outbox.push(&line.row(ts, &namespace, ua, visitor)) {
            tracing::warn!(%error, "cannot hold a request");
        }
    }

    fn tick(&self) {
        if let Err(error) = self.salts.prune(super::ship::unix_seconds()) {
            tracing::warn!(%error, "cannot forget closed salts");
        }
        if let Err(error) = self.outbox.tick() {
            tracing::warn!(%error, "cannot write a request segment");
        }
    }
}

#[derive(Serialize)]
struct Row<'a> {
    ts: i64,
    namespace: &'a Namespace,
    service: &'a str,
    host: String,
    method: &'a str,
    path: &'a str,
    query: BTreeMap<&'static str, String>,
    status: u16,
    duration_us: u64,
    bytes_in: u64,
    bytes_out: u64,
    request_id: &'a str,
    ua: &'a str,
    referrer_host: Option<String>,
    language: Option<String>,
    content_type: Option<&'a str>,
    sec_purpose: Option<&'a str>,
    visitor: Option<String>,
}

/// One line of Caddy's JSON access log, with the fields the `ployz_requests` snippet appends.
#[derive(Deserialize)]
struct Line {
    ts: f64,
    request: Request,
    status: u16,
    /// Seconds, Caddy's default `duration_format`.
    #[serde(default)]
    duration: f64,
    /// Request body bytes read from the client.
    #[serde(default)]
    bytes_read: u64,
    /// Response body bytes written to the client.
    #[serde(default)]
    size: u64,
    #[serde(default)]
    resp_headers: ResponseHeaders,
    namespace: String,
    service: String,
    #[serde(default)]
    request_id: String,
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    client_ip: String,
    method: String,
    host: String,
    uri: String,
    #[serde(default)]
    headers: RequestHeaders,
}

type Values = Option<Vec<String>>;

#[derive(Deserialize, Default)]
struct RequestHeaders {
    #[serde(rename = "User-Agent")]
    user_agent: Values,
    #[serde(rename = "Accept-Language")]
    accept_language: Values,
    #[serde(rename = "Referer")]
    referer: Values,
    #[serde(rename = "Sec-Purpose")]
    sec_purpose: Values,
    #[serde(rename = "Purpose")]
    purpose: Values,
}

#[derive(Deserialize, Default)]
struct ResponseHeaders {
    #[serde(rename = "Content-Type")]
    content_type: Values,
}

impl Line {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Caddy's ts is a Unix time in seconds, far inside i64 milliseconds"
    )]
    fn millis(&self) -> i64 {
        (self.ts * 1000.0).round() as i64
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a request's duration in seconds is non-negative and far inside u64 microseconds"
    )]
    fn duration_us(&self) -> u64 {
        (self.duration * 1_000_000.0).round() as u64
    }

    fn row<'a>(
        &'a self,
        ts: i64,
        namespace: &'a Namespace,
        ua: &'a str,
        visitor: Option<String>,
    ) -> Row<'a> {
        let headers = &self.request.headers;
        let host = bare_host(&self.request.host).to_ascii_lowercase();
        let (path, query) = self
            .request
            .uri
            .split_once('?')
            .unwrap_or((&self.request.uri, ""));
        let referrer_host = first(headers.referer.as_ref())
            .and_then(|referer| url::Url::parse(referer).ok())
            .and_then(|referer| referer.host_str().map(str::to_ascii_lowercase))
            .filter(|referrer| *referrer != host && referrer.len() <= MAX_HOST);
        Row {
            ts,
            namespace,
            service: &self.service,
            host,
            method: cut(&self.request.method, MAX_METHOD),
            path: cut(path, MAX_PATH),
            query: allowed_query(query),
            status: self.status,
            duration_us: self.duration_us(),
            bytes_in: self.bytes_read,
            bytes_out: self.size,
            request_id: cut(&self.request_id, MAX_REQUEST_ID),
            ua: cut(ua, MAX_HEADER),
            referrer_host,
            language: first(headers.accept_language.as_ref()).and_then(primary_language),
            content_type: first(self.resp_headers.content_type.as_ref())
                .map(|value| cut(value, MAX_HEADER)),
            sec_purpose: first(headers.sec_purpose.as_ref())
                .or_else(|| first(headers.purpose.as_ref()))
                .map(|value| cut(value, MAX_HEADER)),
            visitor,
        }
    }
}

fn first(values: Option<&Vec<String>>) -> Option<&str> {
    values?.first().map(String::as_str)
}

fn cut(value: &str, max: usize) -> &str {
    value.get(..value.floor_char_boundary(max)).unwrap_or(value)
}

fn bare_host(host: &str) -> &str {
    if let Some(bracketed) = host.strip_prefix('[') {
        return bracketed
            .find(']')
            .and_then(|end| host.get(..end + 2))
            .unwrap_or(host);
    }
    match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => name,
        Some(_) | None => host,
    }
}

fn allowed_query(query: &str) -> BTreeMap<&'static str, String> {
    let mut kept = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let Some(key) = QUERY_KEYS.iter().find(|allowed| **allowed == key) else {
            continue;
        };
        kept.entry(*key).or_insert_with(|| scrub(&value));
    }
    kept
}

fn scrub(value: &str) -> String {
    if PERSONAL.is_match(value) {
        return "redacted".to_owned();
    }
    cut(value, MAX_QUERY_VALUE).to_owned()
}

fn primary_language(accept_language: &str) -> Option<String> {
    let first = accept_language.split(',').next()?.split(';').next()?;
    let primary = first.trim().split('-').next()?;
    (!primary.is_empty() && primary.len() <= 8 && primary.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| primary.to_ascii_lowercase())
}

fn visitor(salt: &[u8], namespace: &Namespace, client_ip: &str, user_agent: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(salt);
    hash.update(namespace.as_str());
    hash.update([0]);
    hash.update(client_ip);
    hash.update([0]);
    hash.update(user_agent);
    hex::encode(hash.finalize())
}

/// Follows a file Caddy appends to and rolls by renaming.
#[derive(Debug, Clone, Copy)]
enum Start {
    /// The first file, so a restart does not count old requests again.
    End,
    Beginning,
}

struct Tail {
    path: PathBuf,
    file: Option<(File, u64)>,
    start: Start,
    position: u64,
    partial: Vec<u8>,
    skipping: bool,
    chunk: Vec<u8>,
}

impl Tail {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            start: Start::End,
            position: 0,
            partial: Vec::new(),
            skipping: false,
            chunk: vec![0; READ_CHUNK],
        }
    }

    fn poll(&mut self, mut on_line: impl FnMut(&[u8])) -> io::Result<usize> {
        if self.file.is_none() && !self.open()? {
            return Ok(0);
        }
        let read = self.drain(&mut on_line)?;
        if read > 0 {
            return Ok(read);
        }
        let Some((_, inode)) = &self.file else {
            return Ok(0);
        };
        match std::fs::metadata(&self.path) {
            Ok(metadata) if metadata.ino() != *inode => {
                let drained = self.drain(&mut on_line)?;
                self.file = None;
                self.partial.clear();
                self.skipping = false;
                self.open()?;
                Ok(drained)
            }
            Ok(metadata) if metadata.len() < self.position => {
                if let Some((file, _)) = &mut self.file {
                    file.seek(SeekFrom::Start(0))?;
                }
                self.position = 0;
                self.partial.clear();
                self.skipping = false;
                Ok(0)
            }
            Ok(_) => Ok(0),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    fn open(&mut self) -> io::Result<bool> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.start = Start::Beginning;
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        let inode = file.metadata()?.ino();
        self.position = match self.start {
            Start::End => file.seek(SeekFrom::End(0))?,
            Start::Beginning => 0,
        };
        self.start = Start::Beginning;
        self.file = Some((file, inode));
        Ok(true)
    }

    fn drain(&mut self, on_line: &mut impl FnMut(&[u8])) -> io::Result<usize> {
        let Some((file, _)) = &mut self.file else {
            return Ok(0);
        };
        let mut total = 0;
        loop {
            let read = file.read(&mut self.chunk)?;
            if read == 0 {
                return Ok(total);
            }
            total += read;
            self.position += read as u64;
            let mut rest = self.chunk.get(..read).unwrap_or_default();
            while let Some(end) = memchr::memchr(b'\n', rest) {
                let (head, tail) = rest.split_at(end);
                if !std::mem::take(&mut self.skipping) {
                    if self.partial.is_empty() {
                        on_line(head);
                    } else {
                        self.partial.extend_from_slice(head);
                        on_line(&self.partial);
                        self.partial.clear();
                    }
                }
                rest = tail.get(1..).unwrap_or_default();
            }
            if !self.skipping {
                self.partial.extend_from_slice(rest);
                if self.partial.len() > MAX_LINE {
                    self.partial.clear();
                    self.skipping = true;
                }
            }
        }
    }
}

pub(super) fn spawn(
    run_dir: &Path,
    salts: Arc<Salts>,
    outbox: Arc<RequestOutbox>,
) -> io::Result<()> {
    let path = run_dir.join("ingress").join(crate::ingress::ACCESS_LOG);
    std::thread::Builder::new()
        .name("requests".to_owned())
        .spawn(move || tail(&path, &salts, &outbox))?;
    Ok(())
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
