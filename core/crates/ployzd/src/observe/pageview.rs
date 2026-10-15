use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use ployz_core::Namespace;
use regex::RegexSet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    bots,
    salts::{Lookup, Salts},
    ship::PageviewOutbox,
};

const POLL: Duration = Duration::from_millis(200);
const COUNTS_EVERY: Duration = Duration::from_secs(60);
const READ_CHUNK: usize = 64 * 1024;
const MAX_LINE: usize = 1024 * 1024;
const MAX_PATH: usize = 1024;
const MAX_QUERY_VALUE: usize = 256;
const QUERY_KEYS: [&str; 6] = [
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "ref",
];
const CLICK_IDS: [&str; 2] = ["fbclid", "gclid"];

static PERSONAL: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"[^@\s]+@[^@\s]+\.[^@\s]+",
        r"eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]*\.",
        r"[A-Za-z0-9_-]{32,}",
    ])
    .expect("the personal-value patterns compile")
});

pub(super) fn tail(path: &Path, salts: &Salts, outbox: &PageviewOutbox) {
    let mut tail = Tail::new(path.to_path_buf());
    let mut harvest = Harvest::new(salts, outbox);
    loop {
        let read = tail
            .poll(|line| harvest.line(line))
            .unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "cannot read the pageview log");
                0
            });
        harvest.tick(Instant::now());
        if read == 0 {
            std::thread::sleep(POLL);
        }
    }
}

struct Harvest<'a> {
    salts: &'a Salts,
    outbox: &'a PageviewOutbox,
    counts: HashMap<Count, u64>,
    counted_at: Instant,
}

#[derive(Debug, Hash, PartialEq, Eq)]
enum Count {
    Bots {
        namespace: Namespace,
        day: String,
        reason: Cow<'static, str>,
    },
    Gap {
        namespace: Namespace,
    },
}

impl<'a> Harvest<'a> {
    fn new(salts: &'a Salts, outbox: &'a PageviewOutbox) -> Self {
        Self {
            salts,
            outbox,
            counts: HashMap::new(),
            counted_at: Instant::now(),
        }
    }

    fn line(&mut self, bytes: &[u8]) {
        let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
            tracing::debug!("skipping an access log line that does not decode");
            return;
        };
        if !line.is_pageview() {
            return;
        }
        let Some(namespace) = line
            .namespace
            .as_deref()
            .and_then(|namespace| Namespace::parse(namespace).ok())
        else {
            return;
        };
        let ts = line.millis();
        let lookup = self.salts.lookup(&namespace, ts.div_euclid(1000));
        if lookup == Lookup::Off {
            return;
        }
        let headers = &line.request.headers;
        let user_agent = first(headers.user_agent.as_ref()).unwrap_or_default();
        let count = match (lookup, bots::reason(user_agent)) {
            (Lookup::Off, _) | (Lookup::Gap, Some(_)) => return,
            (Lookup::Covered { day, salt }, None) => {
                let row = line.pageview(ts, &day, &namespace, &salt, user_agent);
                if let Err(error) = self.outbox.push(&Row::Pageview(row)) {
                    tracing::warn!(%error, "cannot hold a pageview");
                }
                return;
            }
            (Lookup::Gap, None) => Count::Gap { namespace },
            (Lookup::Covered { day, .. }, Some(reason)) => Count::Bots {
                namespace,
                day,
                reason,
            },
        };
        *self.counts.entry(count).or_default() += 1;
    }

    fn tick(&mut self, now: Instant) {
        if let Err(error) = self.salts.prune(super::ship::unix_seconds()) {
            tracing::warn!(%error, "cannot forget closed salts");
        }
        if now.duration_since(self.counted_at) >= COUNTS_EVERY {
            self.counted_at = now;
            self.flush_counts();
        }
        if let Err(error) = self.outbox.tick() {
            tracing::warn!(%error, "cannot write a pageview segment");
        }
    }

    fn flush_counts(&mut self) {
        for (count, n) in self.counts.drain() {
            let row = match &count {
                Count::Bots {
                    namespace,
                    day,
                    reason,
                } => Row::Bots {
                    namespace,
                    day,
                    reason,
                    count: n,
                },
                Count::Gap { namespace } => Row::Gap {
                    namespace,
                    day: None,
                    count: n,
                },
            };
            if let Err(error) = self.outbox.push(&row) {
                tracing::warn!(%error, "cannot hold a pageview count");
            }
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Row<'a> {
    Pageview(Pageview<'a>),
    Bots {
        namespace: &'a Namespace,
        day: &'a str,
        reason: &'a str,
        count: u64,
    },
    Gap {
        namespace: &'a Namespace,
        day: Option<&'a str>,
        count: u64,
    },
}

#[derive(Serialize)]
struct Pageview<'a> {
    ts: i64,
    day: &'a str,
    namespace: &'a Namespace,
    host: String,
    path: &'a str,
    query: BTreeMap<&'static str, String>,
    referrer_host: Option<String>,
    language: Option<String>,
    visitor: String,
    flags: Vec<&'static str>,
}

#[derive(Deserialize)]
struct Line {
    ts: f64,
    request: Request,
    status: u16,
    #[serde(default)]
    resp_headers: ResponseHeaders,
    namespace: Option<String>,
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
    #[serde(rename = "Sec-Fetch-Site")]
    sec_fetch_site: Values,
    #[serde(rename = "Sec-Fetch-Mode")]
    sec_fetch_mode: Values,
    #[serde(rename = "Sec-Fetch-Dest")]
    sec_fetch_dest: Values,
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
    fn is_pageview(&self) -> bool {
        let headers = &self.request.headers;
        let prefetch = [&headers.sec_purpose, &headers.purpose]
            .into_iter()
            .flatten()
            .flatten()
            .any(|value| value.to_ascii_lowercase().contains("prefetch"));
        self.request.method == "GET"
            && (200..300).contains(&self.status)
            && first(self.resp_headers.content_type.as_ref()).is_some_and(|content_type| {
                content_type
                    .get(.."text/html".len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/html"))
            })
            && !prefetch
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "Caddy's ts is a Unix time in seconds, far inside i64 milliseconds"
    )]
    fn millis(&self) -> i64 {
        (self.ts * 1000.0).round() as i64
    }

    fn pageview<'a>(
        &'a self,
        ts: i64,
        day: &'a str,
        namespace: &'a Namespace,
        salt: &[u8],
        user_agent: &str,
    ) -> Pageview<'a> {
        let headers = &self.request.headers;
        let host = bare_host(&self.request.host).to_ascii_lowercase();
        let (path, query) = self
            .request
            .uri
            .split_once('?')
            .unwrap_or((&self.request.uri, ""));
        let (query, click_id) = allowed_query(query);
        let language = first(headers.accept_language.as_ref()).and_then(primary_language);
        let mut flags = Vec::new();
        if first(headers.accept_language.as_ref()).is_none_or(|value| value.trim().is_empty()) {
            flags.push("no_accept_language");
        }
        if [
            &headers.sec_fetch_site,
            &headers.sec_fetch_mode,
            &headers.sec_fetch_dest,
        ]
        .iter()
        .all(|values| first(values.as_ref()).is_none())
        {
            flags.push("no_sec_fetch");
        }
        if click_id {
            flags.push("click_id");
        }
        let referrer_host = first(headers.referer.as_ref())
            .and_then(|referer| url::Url::parse(referer).ok())
            .and_then(|referer| referer.host_str().map(str::to_ascii_lowercase))
            .filter(|referrer| *referrer != host);
        Pageview {
            ts,
            day,
            namespace,
            host,
            path: path
                .get(..path.floor_char_boundary(MAX_PATH))
                .unwrap_or(path),
            query,
            referrer_host,
            language,
            visitor: visitor(salt, namespace, &self.request.client_ip, user_agent),
            flags,
        }
    }
}

fn first(values: Option<&Vec<String>>) -> Option<&str> {
    values?.first().map(String::as_str)
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

fn allowed_query(query: &str) -> (BTreeMap<&'static str, String>, bool) {
    let mut kept = BTreeMap::new();
    let mut click_id = false;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if CLICK_IDS.contains(&key.as_ref()) {
            click_id = true;
        }
        let Some(key) = QUERY_KEYS.iter().find(|allowed| **allowed == key) else {
            continue;
        };
        kept.entry(*key).or_insert_with(|| scrub(&value));
    }
    (kept, click_id)
}

fn scrub(value: &str) -> String {
    if PERSONAL.is_match(value) {
        return "redacted".to_owned();
    }
    value
        .get(..value.floor_char_boundary(MAX_QUERY_VALUE))
        .unwrap_or(value)
        .to_owned()
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
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
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
    outbox: Arc<PageviewOutbox>,
) -> io::Result<()> {
    let path = run_dir.join("ingress").join(crate::ingress::ACCESS_LOG);
    std::thread::Builder::new()
        .name("pageviews".to_owned())
        .spawn(move || tail(&path, &salts, &outbox))?;
    Ok(())
}

#[cfg(test)]
#[path = "pageview_tests.rs"]
mod tests;
