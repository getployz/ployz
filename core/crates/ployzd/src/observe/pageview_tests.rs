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
const DAY_START: i64 = 1_792_026_000;

struct Lab {
    _root: TestDir,
    store: StoreRoot,
    salts: Salts,
    outbox: Arc<PageviewOutbox>,
}

impl Lab {
    fn new(windows: &[(&str, i64, i64)]) -> Self {
        let root = TestDir::new("ployzd-observe-pageview");
        fs::create_dir_all(&root.0).unwrap();
        let store = StoreRoot::under(&root.0);
        store.prepare().unwrap();
        let salts = Salts::open(&store, 0).unwrap();
        let windows: Vec<_> = windows
            .iter()
            .map(
                |(day, start, end)| json!({ "day": day, "start": start, "end": end, "salt": SALT }),
            )
            .collect();
        let ack = json!({ "sites": [{ "namespace": "acme-prod", "salts": windows }] });
        salts
            .accept(&serde_json::to_vec(&ack).unwrap(), DAY_START)
            .unwrap();
        let outbox = PageviewOutbox::open(&store).unwrap();
        Self {
            _root: root,
            store,
            salts,
            outbox,
        }
    }

    fn today() -> Self {
        Self::new(&[("2026-10-15", DAY_START, DAY_START + 24 * 3600)])
    }

    fn harvest(&self, lines: &[Value]) -> Vec<Value> {
        let mut harvest = Harvest::new(&self.salts, &self.outbox);
        for line in lines {
            harvest.line(&serde_json::to_vec(line).unwrap());
        }
        harvest.flush_counts();
        self.outbox.rows()
    }
}

fn request(uri: &str) -> Value {
    json!({
        "level": "info",
        "ts": DAY_START as f64 + 3600.25,
        "logger": "http.log.access.ployz_pageviews",
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
                "Sec-Fetch-Site": ["cross-site"],
                "Sec-Fetch-Mode": ["navigate"],
                "Sec-Fetch-Dest": ["document"],
            },
        },
        "status": 200,
        "resp_headers": { "Content-Type": ["text/html; charset=utf-8"] },
        "namespace": "acme-prod",
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

fn kinds(rows: &[Value]) -> Vec<&str> {
    rows.iter()
        .map(|row| row["kind"].as_str().unwrap())
        .collect()
}

#[test]
fn a_browser_page_load_becomes_the_contract_row() {
    let lab = Lab::today();
    let rows = lab.harvest(&[request("/pricing?utm_source=hn&utm_source=other&page=2")]);

    let salt: Vec<u8> = (0..32).collect();
    let mut hash = Sha256::new();
    hash.update(&salt);
    hash.update(b"acme-prod\x00203.0.113.77\x00");
    hash.update(UA);
    assert_eq!(
        rows,
        [json!({
            "kind": "pageview",
            "ts": (DAY_START + 3600) * 1000 + 250,
            "day": "2026-10-15",
            "namespace": "acme-prod",
            "host": "acme.com",
            "path": "/pricing",
            "query": { "utm_source": "hn" },
            "referrer_host": "news.ycombinator.com",
            "language": "en",
            "visitor": hex::encode(hash.finalize()),
            "flags": [],
        })]
    );
}

#[test]
fn only_html_gets_answered_2xx_that_are_not_prefetches_count() {
    let lab = Lab::today();
    let page = request("/");
    let rows = lab.harvest(&[
        with(page.clone(), "/request/method", json!("POST")),
        with(page.clone(), "/request/method", json!("HEAD")),
        with(page.clone(), "/status", json!(304)),
        with(page.clone(), "/status", json!(404)),
        with(
            page.clone(),
            "/resp_headers/Content-Type",
            json!(["application/json"]),
        ),
        with(
            page.clone(),
            "/resp_headers/Content-Type",
            json!(["text/plain"]),
        ),
        without(page.clone(), "/resp_headers/Content-Type"),
        with(
            page.clone(),
            "/request/headers/Sec-Purpose",
            json!(["prefetch;prerender"]),
        ),
        with(
            page.clone(),
            "/request/headers/Purpose",
            json!(["prefetch"]),
        ),
        with(
            page.clone(),
            "/resp_headers/Content-Type",
            json!(["TEXT/HTML"]),
        ),
        with(page.clone(), "/status", json!(203)),
        with(
            page.clone(),
            "/request/headers/Sec-Purpose",
            json!(["prerender"]),
        ),
        page,
    ]);
    assert_eq!(kinds(&rows), ["pageview"; 4]);
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
    assert_eq!(rows[0]["flags"], json!([]));
    assert_eq!(rows[1]["query"]["ref"], "a.".repeat(128));
    assert_eq!(rows[1]["query"]["utm_content"], "short-token_12");
    assert_eq!(rows[1]["flags"], json!(["click_id"]));
    assert_eq!(rows[2]["query"], json!({}));
    assert_eq!(rows[2]["flags"], json!(["click_id"]));
}

#[test]
fn missing_headers_flag_the_row_instead_of_dropping_it() {
    let lab = Lab::today();
    let bare = [
        "Accept-Language",
        "Referer",
        "Sec-Fetch-Site",
        "Sec-Fetch-Mode",
        "Sec-Fetch-Dest",
    ]
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
            request(&format!("/{}", "p".repeat(2000))),
            "/request/headers/Referer",
            json!(["not a url"]),
        ),
    ]);
    assert_eq!(kinds(&rows), ["pageview"; 4]);
    assert_eq!(
        rows[0]["flags"],
        json!(["no_accept_language", "no_sec_fetch"])
    );
    assert_eq!(rows[0]["language"], Value::Null);
    assert_eq!(rows[0]["referrer_host"], Value::Null);
    assert_eq!(rows[1]["referrer_host"], Value::Null);
    assert_eq!(rows[2]["language"], Value::Null);
    assert_eq!(rows[2]["flags"], json!([]));
    assert_eq!(rows[3]["path"].as_str().unwrap().len(), 1024);
    assert_eq!(rows[3]["referrer_host"], Value::Null);
}

#[test]
fn a_request_takes_the_salt_whose_window_holds_it_across_a_dst_change() {
    let short = DAY_START;
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
    let days: Vec<_> = rows
        .iter()
        .filter(|row| row["kind"] == "pageview")
        .map(|row| row["day"].as_str().unwrap())
        .collect();
    assert_eq!(
        days,
        ["2026-10-04", "2026-10-04", "2026-10-05", "2026-10-05"]
    );
    assert_eq!(
        rows.iter().find(|row| row["kind"] == "gap"),
        Some(&json!({ "kind": "gap", "namespace": "acme-prod", "day": null, "count": 2 }))
    );
}

#[test]
fn bots_and_gaps_become_counts_and_an_off_site_leaves_no_trace() {
    let lab = Lab::new(&[("2026-10-15", DAY_START, DAY_START + 24 * 3600)]);
    let bot = with(
        request("/"),
        "/request/headers/User-Agent",
        json!(["Googlebot/2.1"]),
    );
    let empty = without(request("/"), "/request/headers/User-Agent");
    let late = with(request("/"), "/ts", json!(DAY_START as f64 + 25.0 * 3600.0));
    let late_bot = with(
        late.clone(),
        "/request/headers/User-Agent",
        json!(["Googlebot/2.1"]),
    );
    let off = with(request("/"), "/namespace", json!("other-prod"));
    let unnamed = without(request("/"), "/namespace");

    let mut harvest = Harvest::new(&lab.salts, &lab.outbox);
    for line in [&bot, &bot, &empty, &late, &late_bot, &off, &unnamed] {
        harvest.line(&serde_json::to_vec(line).unwrap());
    }
    harvest.tick(Instant::now());
    assert_eq!(lab.outbox.rows(), [] as [Value; 0], "counts wait a minute");
    harvest.tick(Instant::now() + COUNTS_EVERY);
    let mut rows = lab.outbox.rows();
    rows.sort_by_key(Value::to_string);
    assert_eq!(
        rows,
        [
            json!({ "kind": "bots", "namespace": "acme-prod", "day": "2026-10-15", "reason": "empty_ua", "count": 1 }),
            json!({ "kind": "gap", "namespace": "acme-prod", "day": null, "count": 1 }),
            json!({ "kind": "bots", "namespace": "acme-prod", "day": "2026-10-15", "reason": "googlebot", "count": 2 }),
        ]
    );
    harvest.tick(Instant::now() + COUNTS_EVERY * 2);
    assert_eq!(
        lab.outbox.rows().len(),
        3,
        "a flush sends deltas, not totals"
    );
}

#[test]
fn no_client_address_or_user_agent_reaches_a_row_or_the_disk() {
    let lab = Lab::today();
    let lines: Vec<_> = (0..150).map(|n| request(&format!("/{n}"))).collect();
    let rows = lab.harvest(&lines);
    assert_eq!(rows.len(), 150);
    let text = serde_json::to_string(&rows).unwrap();
    assert!(!text.contains(IP) && !text.contains("Chrome/130"));
    let outbox = lab.store.pageviews_outbox();
    assert!(dir_has(&outbox, b"/149"), "the segments hold the rows");
    assert!(!dir_has(&outbox, IP.as_bytes()));
    assert!(!dir_has(&outbox, b"Chrome/130"));
}

#[test]
fn the_tail_starts_at_the_end_and_follows_appends_rolls_and_truncation() {
    let root = TestDir::new("ployzd-observe-pageview-tail");
    fs::create_dir_all(&root.0).unwrap();
    let path = root.0.join("access.log");
    let append = |text: &str| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    };
    let mut tail = Tail::new(path.clone());
    let mut poll = || {
        let mut lines = Vec::new();
        tail.poll(|line| lines.push(String::from_utf8(line.to_vec()).unwrap()))
            .unwrap();
        lines
    };

    assert!(poll().is_empty(), "no log yet");
    append("old\n");
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
}
