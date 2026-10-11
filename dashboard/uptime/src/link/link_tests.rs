use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tokio::net::TcpListener;
use uuid::Uuid;

use super::{Gate, HttpLink, PlaneApi, REGION_HEADER, serve};

const REGION: &str = "ployz-probe-region";
use crate::config::Secret;
use crate::domain::{
    AgentName, CheckId, Detail, HttpCheckError, Job, NotProbed, Observation, Outcome, Phase,
    ProbeKey, ProbeKind, Region, Target, Timings, UnixMs,
};
use crate::link::{ControlLink, LinkError, Pulled, Rejected, ResultBatch};
use crate::run::Shutdown;

#[derive(Default)]
struct Fake {
    ready: AtomicBool,
    pulls: AtomicU32,
    wants: Mutex<Vec<u32>>,
    jobs: Mutex<Vec<Job>>,
    reports: Mutex<Vec<ResultBatch>>,
}

impl PlaneApi for Fake {
    async fn pull(&self, _region: &Region, want: u32, _wait: Duration) -> Pulled {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        self.wants.lock().unwrap().push(want);
        Pulled {
            jobs: self.jobs.lock().unwrap().clone(),
            server_now: UnixMs(1_700_000_000_000),
        }
    }

    async fn report(&self, _region: &Region, batch: ResultBatch) -> Result<(), Rejected> {
        self.reports.lock().unwrap().push(batch);
        Ok(())
    }

    fn ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

struct Server {
    url: String,
    plane: Arc<Fake>,
    _trigger: crate::run::Trigger,
}

async fn server(token: Option<&str>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let plane = Arc::new(Fake::default());
    let (trigger, shutdown) = Shutdown::new();
    let gate = Gate {
        token: token.map(Secret::new),
        regions: vec![region("fra")],
    };
    tokio::spawn(serve(listener, plane.clone(), gate, shutdown));
    Server {
        url,
        plane,
        _trigger: trigger,
    }
}

fn region(name: &str) -> Region {
    Region::parse(name).unwrap()
}

fn job(seed: u128) -> Job {
    Job {
        key: ProbeKey {
            check: CheckId(Uuid::from_u128(seed)),
            region: region("fra"),
            scheduled_at: UnixMs(1_700_000_000_000),
            kind: ProbeKind::Routine,
        },
        target: Arc::new(Target::parse("https://example.com/health?x=1").unwrap()),
        timeout: Duration::from_millis(1500),
        address: 7,
    }
}

fn observation(outcome: Outcome) -> Observation {
    Observation {
        key: job(1).key,
        started_at: UnixMs(1_700_000_000_500),
        outcome,
        timings: Timings {
            dns_ms: 1,
            connect_ms: 2,
            tls_ms: 3,
            ttfb_ms: 4,
            total_ms: 5,
        },
        remote_ip: Some(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))),
        cert_not_after: Some(1_800_000_000),
        agent: AgentName("fra/r-1".into()),
    }
}

async fn post(url: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (StatusCode, Bytes) {
    let client: Client<HttpConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    let mut request = Request::post(format!("{url}{path}"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_owned())))
        .unwrap();
    let response = client.request(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, body)
}

async fn get(url: &str, path: &str) -> StatusCode {
    let client: Client<HttpConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    let request = Request::get(format!("{url}{path}"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    client.request(request).await.unwrap().status()
}

#[tokio::test]
async fn a_pull_without_the_token_is_refused_before_the_plane_hears_of_it() {
    let server = server(Some("hunter2")).await;
    let region_header = (REGION, "fra");
    let (status, _) = post(&server.url, "/v1/agent/pull", &[region_header], "{}").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let wrong = (AUTHORIZATION.as_str(), "Bearer hunter3");
    let (status, _) = post(&server.url, "/v1/agent/pull", &[region_header, wrong], "{}").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let right = (AUTHORIZATION.as_str(), "Bearer hunter2");
    let (status, _) = post(&server.url, "/v1/agent/pull", &[region_header, right], "{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(server.plane.pulls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_pull_from_an_unknown_region_is_refused() {
    let server = server(None).await;
    let (status, _) = post(&server.url, "/v1/agent/pull", &[], "{}").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let other = (REGION, "sjc");
    let (status, _) = post(&server.url, "/v1/agent/pull", &[other], "{}").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(server.plane.pulls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn jobs_cross_the_link_intact_and_the_server_clock_comes_with_them() {
    let server = server(Some("hunter2")).await;
    server.plane.jobs.lock().unwrap().push(job(1));
    let link = HttpLink::new(&server.url, Some(Secret::new("hunter2")), region("fra")).unwrap();
    let pulled = link.pull(3).await.unwrap();
    assert_eq!(pulled.server_now, UnixMs(1_700_000_000_000));
    assert_eq!(pulled.jobs.len(), 1);
    let got = pulled.jobs.first().unwrap();
    let want = job(1);
    assert_eq!(got.key, want.key);
    assert_eq!(*got.target, *want.target);
    assert_eq!(got.timeout, want.timeout);
    assert_eq!(got.address, want.address);
    assert_eq!(*server.plane.wants.lock().unwrap(), vec![3]);
}

#[tokio::test]
async fn every_outcome_kind_survives_the_round_trip_with_its_batch_id() {
    let server = server(None).await;
    let link = HttpLink::new(&server.url, None, region("fra")).unwrap();
    let observations = vec![
        observation(Outcome::Http { status: 503 }),
        observation(Outcome::Unreachable {
            phase: Phase::Tls,
            error: HttpCheckError::TimedOut,
            detail: Detail::new("handshake took too long"),
        }),
        observation(Outcome::NotProbed(NotProbed::PrivateAddress)),
    ];
    let batch = ResultBatch {
        id: Uuid::now_v7(),
        observations: observations.clone(),
    };
    link.report(&batch).await.unwrap();
    let reports = server.plane.reports.lock().unwrap();
    assert_eq!(reports.len(), 1);
    let got = reports.first().unwrap();
    assert_eq!(got.id, batch.id);
    assert_eq!(got.observations, observations);
}

#[tokio::test]
async fn an_oversized_batch_is_refused_and_not_retried() {
    let server = server(None).await;
    let link = HttpLink::new(&server.url, None, region("fra")).unwrap();
    let batch = ResultBatch {
        id: Uuid::now_v7(),
        observations: (0..5001)
            .map(|_| observation(Outcome::Http { status: 200 }))
            .collect(),
    };
    let err = link.report(&batch).await.unwrap_err();
    assert!(
        matches!(err, LinkError::Rejected(Rejected::Status(413))),
        "{err}"
    );
    assert!(server.plane.reports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_greedy_want_is_capped_before_it_reaches_the_plane() {
    let server = server(None).await;
    let link = HttpLink::new(&server.url, None, region("fra")).unwrap();
    link.pull(10_000).await.unwrap();
    assert_eq!(*server.plane.wants.lock().unwrap(), vec![4096]);
}

#[tokio::test]
async fn unknown_fields_are_ignored_and_malformed_jobs_dropped_and_counted() {
    let server = server(None).await;
    let region_header = (REGION, "fra");
    let (status, _) = post(
        &server.url,
        "/v1/agent/pull",
        &[region_header],
        "{\"want\":1,\"color\":\"red\"}",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let body = concat!(
            "{\"server_now\":5,\"jobs\":[",
            "{\"check\":\"nope\",\"region\":\"fra\",\"scheduled_at\":1,\"kind\":\"routine\",",
            "\"url\":\"https://example.com/\",\"timeout_ms\":10,\"address\":0},",
            "{\"check\":\"00000000-0000-0000-0000-000000000001\",\"region\":\"fra\",",
            "\"scheduled_at\":1,\"kind\":\"routine\",\"url\":\"https://example.com/\",",
            "\"timeout_ms\":10,\"address\":0,\"extra\":true}]}"
        );
        let service = hyper::service::service_fn(move |_req: Request<hyper::body::Incoming>| {
            let body = body.to_owned();
            async move {
                Ok::<_, std::convert::Infallible>(http::Response::new(Full::new(Bytes::from(body))))
            }
        });
        hyper::server::conn::http1::Builder::new()
            .serve_connection(io, service)
            .await
            .unwrap();
    });
    let link = HttpLink::new(&url, None, region("fra")).unwrap();
    let pulled = link.pull(2).await.unwrap();
    assert_eq!(pulled.jobs.len(), 1);
    assert_eq!(pulled.server_now, UnixMs(5));
    assert_eq!(link.malformed_jobs(), 1);
}

#[tokio::test]
async fn ready_answers_503_until_the_plane_has_its_checks() {
    let server = server(Some("hunter2")).await;
    assert_eq!(
        get(&server.url, "/ready").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    server.plane.ready.store(true, Ordering::SeqCst);
    assert_eq!(get(&server.url, "/ready").await, StatusCode::OK);
    assert_eq!(get(&server.url, "/nothing").await, StatusCode::NOT_FOUND);
}

#[test]
fn the_link_refuses_a_control_url_that_is_not_plain_http() {
    let Err(err) = HttpLink::new("https://control", None, region("fra")) else {
        panic!("an https control url was accepted");
    };
    assert!(err.0.contains("UPTIME_CONTROL_URL"), "{}", err.0);
    assert!(!err.0.contains("https://control"), "{}", err.0);
    assert_eq!(REGION_HEADER.as_str(), REGION);
}
