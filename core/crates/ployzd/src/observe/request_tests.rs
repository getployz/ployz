#![expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]

use std::{fs, io::Write};

use serde_json::{Value, json};

use super::*;
use crate::{
    observe::{layout::StoreRoot, ship::dir_has},
    test_dir::TestDir,
};

const SALT: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
const IP: &str = "203.0.113.77";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/130.0 Safari/537.36";
const REQUEST_ID: &str = "b4150e5b-03fe-4cf0-a247-66da4b50c84a";
/// A line Caddy 2.11.4 wrote for a POST through `reverse_proxy` behind the `ployz_requests` snippet.
const RECORDED: &str = r#"{"level":"info","ts":1792135934.0080822,"logger":"http.log.access.ployz_requests","msg":"handled request","request":{"remote_ip":"172.17.0.1","remote_port":"39548","client_ip":"172.17.0.1","proto":"HTTP/1.1","method":"POST","host":"127.0.0.1:18080","uri":"/pricing?utm_source=hn&email=a@b.com","headers":{"Content-Length":["5"],"Content-Type":["application/x-www-form-urlencoded"],"Accept":["*/*"],"User-Agent":["Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0"],"Accept-Language":["en-AU,en;q=0.9"],"Referer":["https://news.example.com/x?y=1"],"Sec-Purpose":["prefetch"]}},"bytes_read":5,"user_id":"","duration":0.001074433,"size":11,"status":200,"resp_headers":{"Via":["1.1 Caddy"],"Content-Length":["11"],"Content-Type":["text/plain; charset=utf-8"],"Server":["Caddy"],"Date":["Fri, 16 Oct 2026 07:32:14 GMT"],"Ployz-Request-Id":["b4150e5b-03fe-4cf0-a247-66da4b50c84a"]},"uuid":"b4150e5b-03fe-4cf0-a247-66da4b50c84a","service":"web","namespace":"app","request_id":"b4150e5b-03fe-4cf0-a247-66da4b50c84a"}"#;

fn day_start() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00+01:00")
        .unwrap()
        .timestamp()
}

struct Lab {
    _root: TestDir,
    store: StoreRoot,
    salts: Salts,
    outbox: Arc<RequestOutbox>,
}

impl Lab {
    fn new(windows: &[(&str, i64, i64)]) -> Self {
        let root = TestDir::new("ployzd-observe-request");
        fs::create_dir_all(&root.0).unwrap();
        let store = StoreRoot::under(&root.0);
        store.prepare().unwrap();
        let salts = Salts::open(&store, 0);
        let windows: Vec<_> = windows
            .iter()
            .map(
                |(day, start, end)| json!({ "day": day, "start": start, "end": end, "salt": SALT }),
            )
            .collect();
        let ack = json!({ "sites": [{ "namespace": "acme-prod", "salts": windows }] });
        salts.accept(&serde_json::to_vec(&ack).unwrap(), day_start());
        let outbox = RequestOutbox::open(&store).unwrap();
        Self {
            _root: root,
            store,
            salts,
            outbox,
        }
    }

    fn today() -> Self {
        Self::new(&[("2026-10-15", day_start(), day_start() + 24 * 3600)])
    }

    fn harvest_raw(&self, lines: &[Vec<u8>]) -> Vec<Value> {
        let harvest = Harvest {
            salts: &self.salts,
            outbox: &self.outbox,
        };
        for line in lines {
            harvest.line(line);
        }
        self.outbox.rows()
    }

    fn harvest(&self, lines: &[Value]) -> Vec<Value> {
        let lines: Vec<_> = lines
            .iter()
            .map(|line| serde_json::to_vec(line).unwrap())
            .collect();
        self.harvest_raw(&lines)
    }
}

fn request(uri: &str) -> Value {
    json!({
        "level": "info",
        "ts": day_start() as f64 + 3600.25,
        "logger": "http.log.access.ployz_requests",
        "msg": "handled request",
        "request": {
            "remote_ip": "10.0.0.1",
            "client_ip": IP,
            "proto": "HTTP/2.0",
            "method": "GET",
            "host": "Acme.com:443",
            "uri": uri,
            "headers": {
                "User-Agent": [UA],
                "Accept-Language": ["en-AU,en;q=0.9"],
                "Referer": ["https://news.ycombinator.com/item?id=1"],
            },
        },
        "bytes_read": 0,
        "duration": 0.0182,
        "size": 48213,
        "status": 200,
        "resp_headers": { "Content-Type": ["text/html; charset=utf-8"] },
        "namespace": "acme-prod",
        "service": "web",
        "request_id": REQUEST_ID,
    })
}

fn with(mut line: Value, pointer: &str, value: Value) -> Value {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    line.pointer_mut(parent).unwrap()[key] = value;
    line
}

fn without(mut line: Value, pointer: &str) -> Value {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    line.pointer_mut(parent)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove(key);
    line
}

fn acme_visitor() -> String {
    let salt: Vec<u8> = (0..32).collect();
    let mut hash = Sha256::new();
    hash.update(&salt);
    hash.update(b"acme-prod\x00203.0.113.77\x00");
    hash.update(UA);
    hex::encode(hash.finalize())
}

#[test]
fn a_request_becomes_the_contract_row() {
    let lab = Lab::today();
    let rows = lab.harvest(&[request("/pricing?utm_source=hn&utm_source=other&page=2")]);
    assert_eq!(
        rows,
        [json!({
            "ts": (day_start() + 3600) * 1000 + 250,
            "namespace": "acme-prod",
            "service": "web",
            "host": "acme.com",
            "method": "GET",
            "path": "/pricing",
            "query": { "utm_source": "hn" },
            "status": 200,
            "duration_us": 18_200,
            "bytes_in": 0,
            "bytes_out": 48_213,
            "request_id": REQUEST_ID,
            "ua": UA,
            "referrer_host": "news.ycombinator.com",
            "language": "en",
            "content_type": "text/html; charset=utf-8",
            "sec_purpose": null,
            "visitor": acme_visitor(),
        })]
    );
}

#[test]
fn a_line_caddy_wrote_decodes_its_duration_and_body_sizes() {
    let lab = Lab::new(&[]);
    let rows = lab.harvest_raw(&[RECORDED.as_bytes().to_vec()]);
    assert_eq!(
        rows,
        [json!({
            "ts": 1_792_135_934_008_i64,
            "namespace": "app",
            "service": "web",
            "host": "127.0.0.1",
            "method": "POST",
            "path": "/pricing",
            "query": { "utm_source": "hn" },
            "status": 200,
            "duration_us": 1074,
            "bytes_in": 5,
            "bytes_out": 11,
            "request_id": REQUEST_ID,
            "ua": "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0",
            "referrer_host": "news.example.com",
            "language": "en",
            "content_type": "text/plain; charset=utf-8",
            "sec_purpose": "prefetch",
            "visitor": null,
        })]
    );
}

#[test]
fn every_request_ships_whatever_its_method_status_or_content_type() {
    let lab = Lab::today();
    let page = request("/");
    let rows = lab.harvest(&[
        with(page.clone(), "/request/method", json!("POST")),
        with(page.clone(), "/request/method", json!("HEAD")),
        with(page.clone(), "/status", json!(304)),
        with(page.clone(), "/status", json!(502)),
        with(
            page.clone(),
            "/resp_headers/Content-Type",
            json!(["application/json"]),
        ),
        without(page.clone(), "/resp_headers/Content-Type"),
        with(
            page.clone(),
            "/request/headers/Sec-Purpose",
            json!(["prefetch;prerender"]),
        ),
        with(page, "/request/headers/Purpose", json!(["prefetch"])),
    ]);
    let shape = |row: &Value| {
        [
            row["method"].clone(),
            row["status"].clone(),
            row["content_type"].clone(),
            row["sec_purpose"].clone(),
        ]
    };
    let html = json!("text/html; charset=utf-8");
    assert_eq!(
        rows.iter().map(shape).collect::<Vec<_>>(),
        [
            [json!("POST"), json!(200), html.clone(), Value::Null],
            [json!("HEAD"), json!(200), html.clone(), Value::Null],
            [json!("GET"), json!(304), html.clone(), Value::Null],
            [json!("GET"), json!(502), html.clone(), Value::Null],
            [
                json!("GET"),
                json!(200),
                json!("application/json"),
                Value::Null
            ],
            [json!("GET"), json!(200), Value::Null, Value::Null],
            [
                json!("GET"),
                json!(200),
                html.clone(),
                json!("prefetch;prerender")
            ],
            [json!("GET"), json!(200), html, json!("prefetch")],
        ]
    );
}

#[test]
fn the_query_keeps_only_campaign_keys_and_redacts_what_looks_personal() {
    let lab = Lab::today();
    let long = "a.".repeat(150);
    let rows = lab.harvest(&[
        request(
            "/?utm_source=jane%40example.com&utm_medium=eyJhbGciOi.eyJzdWIiOi.c2ln\
             &utm_campaign=abcdefghijklmnopqrstuvwxyz012345&utm_term=spring+sale\
             &email=jane@example.com&token=x",
        ),
        request(&format!(
            "/?ref={long}&utm_content=short-token_12&fbclid=IwAR"
        )),
        request("/?gclid=Cj0"),
    ]);
    assert_eq!(
        rows[0]["query"],
        json!({
            "utm_source": "redacted",
            "utm_medium": "redacted",
            "utm_campaign": "redacted",
            "utm_term": "spring sale",
        })
    );
    assert_eq!(rows[1]["query"]["ref"], "a.".repeat(128));
    assert_eq!(rows[1]["query"]["utm_content"], "short-token_12");
    assert_eq!(rows[2]["query"], json!({}));
}

#[test]
fn missing_headers_ship_as_nulls_and_long_values_are_cut() {
    let lab = Lab::today();
    let bare = ["User-Agent", "Accept-Language", "Referer"]
        .into_iter()
        .fold(request("/a"), |line, header| {
            without(line, &format!("/request/headers/{header}"))
        });
    let rows = lab.harvest(&[
        bare,
        with(
            request("/b"),
            "/request/headers/Referer",
            json!(["https://ACME.com/pricing"]),
        ),
        with(
            request("/c"),
            "/request/headers/Accept-Language",
            json!(["*"]),
        ),
        with(
            with(
                request(&format!("/{}", "p".repeat(2000))),
                "/request/headers/Referer",
                json!(["not a url"]),
            ),
            "/request/headers/User-Agent",
            json!(["é".repeat(400)]),
        ),
        without(without(request("/d"), "/duration"), "/request_id"),
    ]);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["ua"], "");
    assert_eq!(rows[0]["language"], Value::Null);
    assert_eq!(rows[0]["referrer_host"], Value::Null);
    assert_eq!(
        rows[1]["referrer_host"],
        Value::Null,
        "a same-site referrer"
    );
    assert_eq!(rows[2]["language"], Value::Null);
    assert_eq!(rows[3]["path"].as_str().unwrap().len(), 1024);
    assert_eq!(rows[3]["referrer_host"], Value::Null);
    assert_eq!(rows[3]["ua"], "é".repeat(256));
    assert_eq!(rows[4]["duration_us"], 0);
    assert_eq!(rows[4]["request_id"], "");
}

#[test]
fn a_referrer_host_longer_than_dns_allows_is_nulled() {
    let lab = Lab::today();
    let label = "a".repeat(63);
    let longest = format!("{label}.{label}.{label}.{}", "b".repeat(61));
    let longer = format!("{longest}b");
    let rows = lab.harvest(&[
        with(
            request("/a"),
            "/request/headers/Referer",
            json!([format!("https://{longest}/")]),
        ),
        with(
            request("/b"),
            "/request/headers/Referer",
            json!([format!("https://{longer}/")]),
        ),
    ]);
    assert_eq!(rows[0]["referrer_host"], longest.as_str());
    assert_eq!(rows[1]["referrer_host"], Value::Null);
}

#[test]
fn a_request_outside_every_salt_window_ships_without_a_visitor() {
    let short = day_start();
    let long = short + 23 * 3600;
    let after = long + 25 * 3600;
    let lab = Lab::new(&[("2026-10-04", short, long), ("2026-10-05", long, after)]);
    let at = |seconds: i64, millis: i64| {
        with(
            request("/"),
            "/ts",
            json!(seconds as f64 + millis as f64 / 1000.0),
        )
    };
    let rows = lab.harvest(&[
        at(short, 0),
        at(long - 1, 999),
        at(long, 0),
        at(after - 1, 999),
        at(after, 0),
        at(short - 1, 999),
    ]);
    let visitor = json!(acme_visitor());
    assert_eq!(
        rows.iter()
            .map(|row| row["visitor"].clone())
            .collect::<Vec<_>>(),
        [
            visitor.clone(),
            visitor.clone(),
            visitor.clone(),
            visitor,
            Value::Null,
            Value::Null,
        ]
    );
}

#[test]
fn bots_and_namespaces_without_salts_ship_and_unattributed_lines_do_not() {
    let lab = Lab::today();
    let bot = with(
        request("/"),
        "/request/headers/User-Agent",
        json!(["Googlebot/2.1"]),
    );
    let off = with(request("/"), "/namespace", json!("other-prod"));
    let unnamed = without(request("/"), "/namespace");
    let invalid = with(request("/"), "/namespace", json!("Not A Namespace"));
    let no_service = without(request("/"), "/service");

    let rows = lab.harvest(&[bot, off, unnamed, invalid, no_service]);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["ua"], "Googlebot/2.1");
    assert_eq!(rows[0]["visitor"].as_str().unwrap().len(), 64);
    assert_eq!(rows[1]["namespace"], "other-prod");
    assert_eq!(rows[1]["visitor"], Value::Null);
}

#[test]
fn no_client_address_reaches_a_row_or_the_disk() {
    let lab = Lab::today();
    let lines: Vec<_> = (0..150).map(|n| request(&format!("/{n}"))).collect();
    let rows = lab.harvest(&lines);
    assert_eq!(rows.len(), 150);
    assert!(!serde_json::to_string(&rows).unwrap().contains(IP));
    let outbox = lab.store.requests_outbox();
    assert!(dir_has(&outbox, b"/149"), "the segments hold the rows");
    assert!(dir_has(&outbox, b"Chrome/130"), "the user agent ships");
    assert!(!dir_has(&outbox, IP.as_bytes()));
}

fn append(path: &Path, text: &str) {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

fn lines(tail: &mut Tail) -> Vec<String> {
    let mut lines = Vec::new();
    tail.poll(|line| lines.push(String::from_utf8(line.to_vec()).unwrap()))
        .unwrap();
    lines
}

#[test]
fn the_tail_starts_at_the_end_and_follows_appends_rolls_and_truncation() {
    let root = TestDir::new("ployzd-observe-request-tail");
    fs::create_dir_all(&root.0).unwrap();
    let path = root.0.join("access.log");
    let append = |text: &str| append(&path, text);
    append("old\n");
    let mut tail = Tail::new(path.clone());
    let mut poll = || lines(&mut tail);

    assert!(
        poll().is_empty(),
        "a log found at start is read from its end"
    );
    append("a\nb");
    assert_eq!(poll(), ["a"]);
    append("c\n");
    assert_eq!(poll(), ["bc"]);

    append("last before roll\n");
    fs::rename(&path, root.0.join("access-1.log")).unwrap();
    append("first after roll\n");
    assert_eq!(poll(), ["last before roll"]);
    assert_eq!(poll(), [] as [&str; 0]);
    assert_eq!(poll(), ["first after roll"]);

    fs::write(&path, "").unwrap();
    assert!(poll().is_empty());
    append("after truncation\n");
    assert_eq!(poll(), ["after truncation"]);

    append(&"x".repeat(MAX_LINE + READ_CHUNK));
    append("\nkept\n");
    assert_eq!(poll(), ["kept"], "an overlong line is skipped");

    append(&"x".repeat(MAX_LINE + READ_CHUNK));
    assert!(poll().is_empty());
    fs::write(&path, "").unwrap();
    assert!(poll().is_empty());
    append("after truncating an overlong line\n");
    assert_eq!(poll(), ["after truncating an overlong line"]);
}

#[test]
fn a_log_that_appears_after_startup_is_read_from_its_first_line() {
    let root = TestDir::new("ployzd-observe-request-tail-cold");
    fs::create_dir_all(&root.0).unwrap();
    let path = root.0.join("access.log");
    let mut tail = Tail::new(path.clone());
    assert!(lines(&mut tail).is_empty(), "no log yet");
    append(&path, "first\nsecond\n");
    assert_eq!(lines(&mut tail), ["first", "second"]);
}
