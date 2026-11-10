#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_core::{
    DeviceKey, MachineId, Namespace, SealingPublicKey,
    authority::{
        CommitSha, RuleId, SignerKey,
        rule::{Builder, RuleEvent, RuleSource, Runner},
    },
};
use ring::hmac;
use serde_json::{Value, json};

use super::*;
use crate::github::test_support::{TEST_KID, encode, keys_with_test_signer, signed_by_test_signer};

const TOKEN: &str = include_str!("fixtures/token.jwt");
const JWKS: &[u8] = include_bytes!("fixtures/jwks.json");
const CLAIMS: &str = include_str!("fixtures/claims.json");

fn token() -> &'static str {
    TOKEN.trim()
}

fn github_keys() -> KeySet {
    KeySet::parse(JWKS).unwrap()
}

fn claims_value() -> Value {
    serde_json::from_str(CLAIMS).unwrap()
}

fn at_seconds(seconds: u64) -> Timestamp {
    Timestamp::from_millis(seconds * 1000).unwrap()
}

fn issued_at() -> Timestamp {
    at_seconds(claims_value()["iat"].as_u64().unwrap())
}

fn expires_at() -> Timestamp {
    at_seconds(claims_value()["exp"].as_u64().unwrap())
}

fn parts(token: &str) -> Vec<String> {
    token.split('.').map(str::to_owned).collect()
}

fn header_of(token: &str) -> Value {
    let header = parts(token).into_iter().next().unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).unwrap()).unwrap()
}

#[test]
fn the_real_token_verifies_at_its_issue_time() {
    let claims = verify(token(), &github_keys(), issued_at()).unwrap();
    let expected: GithubClaims = serde_json::from_str(CLAIMS).unwrap();
    assert_eq!(claims, expected);
    assert_eq!(claims.iss, ISSUER);
    assert_eq!(claims.aud, "ployz-d2-fixture-dummy");
    assert_eq!(claims.repository_id, 1_450_985_696);
    assert_eq!(claims.exp.as_millis() - claims.iat.as_millis(), 300_000);
}

#[test]
fn one_changed_byte_in_any_part_is_refused() {
    for part in 0..3 {
        let mut changed = parts(token());
        let text = changed.get_mut(part).unwrap();
        let middle = text.len() / 2;
        let byte = text.as_bytes().get(middle).copied().unwrap();
        let other = if byte == b'A' { "B" } else { "A" };
        text.replace_range(middle..=middle, other);
        let changed = changed.join(".");
        assert!(
            verify(&changed, &github_keys(), issued_at()).is_err(),
            "part {part} changed"
        );
    }
}

#[test]
fn a_key_id_the_issuer_does_not_publish_is_refused() {
    assert!(matches!(
        verify(token(), &KeySet::default(), issued_at()),
        Err(TokenRefusal::UnknownKey)
    ));
    let mut header = header_of(token());
    header["kid"] = json!("not-a-github-key");
    let mut renamed = parts(token());
    *renamed.first_mut().unwrap() = encode(&header);
    assert!(matches!(
        verify(&renamed.join("."), &github_keys(), issued_at()),
        Err(TokenRefusal::UnknownKey)
    ));
}

#[test]
fn another_issuer_is_refused_even_signed_by_a_trusted_key() {
    let keys = keys_with_test_signer();
    let honest = signed_by_test_signer(TEST_KID, &claims_value());
    assert!(verify(&honest, &keys, issued_at()).is_ok());
    let mut claims = claims_value();
    claims["iss"] = json!("https://token.actions.githubusercontent.com.example.com");
    let other = signed_by_test_signer(TEST_KID, &claims);
    assert!(matches!(
        verify(&other, &keys, issued_at()),
        Err(TokenRefusal::Issuer)
    ));
}

#[test]
fn a_token_naming_githubs_key_but_signed_by_another_is_refused() {
    let kid = header_of(token())["kid"].as_str().unwrap().to_owned();
    let forged = signed_by_test_signer(&kid, &claims_value());
    assert!(matches!(
        verify(&forged, &github_keys(), issued_at()),
        Err(TokenRefusal::Signature)
    ));
}

#[test]
fn a_token_outside_its_lifetime_is_refused() {
    let skew = CLOCK_SKEW_MILLIS;
    assert!(verify(token(), &github_keys(), expires_at().plus(skew)).is_ok());
    assert!(matches!(
        verify(token(), &github_keys(), expires_at().plus(skew + 1)),
        Err(TokenRefusal::Lifetime(AuthorityRefusal::Expired))
    ));
    let early = Timestamp::from_millis(issued_at().as_millis() - skew - 1).unwrap();
    assert!(matches!(
        verify(token(), &github_keys(), early),
        Err(TokenRefusal::Lifetime(AuthorityRefusal::Mismatch { .. }))
    ));
    assert!(matches!(
        verify(token(), &github_keys(), Timestamp::now()),
        Err(TokenRefusal::Lifetime(AuthorityRefusal::Expired))
    ));
}

#[test]
fn alg_none_is_refused() {
    let mut header = header_of(token());
    header["alg"] = json!("none");
    let payload = parts(token()).get(1).cloned().unwrap();
    for signature in ["", "AAAA"] {
        let unsigned = format!("{}.{payload}.{signature}", encode(&header));
        assert!(matches!(
            verify(&unsigned, &github_keys(), issued_at()),
            Err(TokenRefusal::Algorithm)
        ));
    }
}

/// The classic key confusion: HMAC keyed with the public modulus, for a verifier that
/// takes the algorithm from the token.
#[test]
fn hs256_signed_with_the_published_modulus_is_refused() {
    let mut header = header_of(token());
    let kid = header["kid"].as_str().unwrap().to_owned();
    let jwks: Value = serde_json::from_slice(JWKS).unwrap();
    let modulus = jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["kid"] == json!(kid))
        .and_then(|key| key["n"].as_str())
        .unwrap()
        .to_owned();
    header["alg"] = json!("HS256");
    let payload = parts(token()).get(1).cloned().unwrap();
    let signed = format!("{}.{payload}", encode(&header));
    for secret in [
        URL_SAFE_NO_PAD.decode(&modulus).unwrap(),
        modulus.into_bytes(),
    ] {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &secret);
        let tag = hmac::sign(&key, signed.as_bytes());
        let forged = format!("{signed}.{}", URL_SAFE_NO_PAD.encode(tag.as_ref()));
        assert!(matches!(
            verify(&forged, &github_keys(), issued_at()),
            Err(TokenRefusal::Algorithm)
        ));
    }
}

#[test]
fn shapes_that_are_not_a_jwt_are_refused() {
    let token = token();
    for bad in [
        String::new(),
        "a.b".to_owned(),
        format!("{token}.extra"),
        "!!!.e30.".to_owned(),
        "x".repeat(MAX_TOKEN_BYTES + 1),
    ] {
        assert!(verify(&bad, &github_keys(), issued_at()).is_err());
    }
}

/// A rule the fixture token's claims match.
fn fixture_rule() -> AutoDeployRule {
    let claims = claims_value();
    let text = |name: &str| claims[name].as_str().unwrap().to_owned();
    AutoDeployRule {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        rule: RuleId::from_bytes([0x5a; 16]),
        generation: 1,
        namespace: Namespace::parse("lab").unwrap(),
        machines: vec![MachineId::parse("1".repeat(32)).unwrap()],
        source: RuleSource {
            repository_id: text("repository_id").parse().unwrap(),
            owner_id: text("repository_owner_id").parse().unwrap(),
            repository: text("repository"),
            branch: "main".to_owned(),
            workflow: ".github/workflows/oidc-probe.yml".to_owned(),
            events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
            builder: Builder {
                r#ref: text("job_workflow_ref"),
                sha: CommitSha::parse(&text("job_workflow_sha")).unwrap(),
            },
            runner: Runner::GithubHosted,
        },
        services: Vec::new(),
        issued_at: Timestamp::from_millis(0).unwrap(),
        ended: false,
        signer: SignerKey::Device(DeviceKey::from_bytes([1; 32])),
    }
}

/// A JWKS host that serves `bodies` in turn, the last one from then on, and counts
/// requests.
async fn jwks_host(bodies: Vec<Vec<u8>>) -> (Url, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let served = Arc::clone(&hits);
    let app = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let at = served.fetch_add(1, Ordering::SeqCst);
            let body = bodies
                .get(at)
                .or_else(|| bodies.last())
                .cloned()
                .unwrap_or_default();
            async move { body }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (Url::parse(&format!("http://{address}/jwks")).unwrap(), hits)
}

fn keys_at(url: Url, refetch_floor: Duration) -> IssuerKeys {
    IssuerKeys::with(reqwest::Client::new(), url, refetch_floor)
}

#[tokio::test]
async fn keys_are_cached_and_fetched_again_once_for_an_unknown_key_id() {
    let empty = br#"{"keys":[]}"#.to_vec();
    let (url, hits) = jwks_host(vec![empty, JWKS.to_vec()]).await;
    let keys = keys_at(url, Duration::ZERO);

    assert!(matches!(
        keys.verify(token(), issued_at()).await,
        Err(TokenRefusal::UnknownKey)
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the first use fetches");

    assert!(keys.verify(token(), issued_at()).await.is_ok());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "a rotated key fetches once more"
    );

    assert!(keys.verify(token(), issued_at()).await.is_ok());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "a known key is served from the cache"
    );

    let mut header = header_of(token());
    header["kid"] = json!("made-up");
    let mut made_up = parts(token());
    *made_up.first_mut().unwrap() = encode(&header);
    assert!(matches!(
        keys.verify(&made_up.join("."), issued_at()).await,
        Err(TokenRefusal::UnknownKey)
    ));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        3,
        "an unknown key fetches exactly once"
    );
}

#[tokio::test]
async fn unknown_key_ids_cannot_make_the_machine_refetch_within_a_minute() {
    let (url, hits) = jwks_host(vec![JWKS.to_vec()]).await;
    let keys = keys_at(url, REFETCH_FLOOR);
    assert!(keys.verify(token(), issued_at()).await.is_ok());
    let mut header = header_of(token());
    for at in 0..5 {
        header["kid"] = json!(format!("made-up-{at}"));
        let mut made_up = parts(token());
        *made_up.first_mut().unwrap() = encode(&header);
        assert!(keys.verify(&made_up.join("."), issued_at()).await.is_err());
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_key_set_that_does_not_answer_is_a_keys_refusal() {
    let (url, _) = jwks_host(vec![b"not json".to_vec()]).await;
    assert!(matches!(
        keys_at(url, Duration::ZERO)
            .verify(token(), issued_at())
            .await,
        Err(TokenRefusal::Keys(KeysError::Malformed))
    ));
}

#[derive(Clone, Default)]
struct Captured(Arc<StdMutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for Captured {
    type Writer = Self;
    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_refusal_logs_the_claim_and_the_rule_never_the_token() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);
    let (url, _) = jwks_host(vec![JWKS.to_vec()]).await;
    let keys = keys_at(url, Duration::ZERO);

    assert!(
        keys.check(&fixture_rule(), token(), issued_at())
            .await
            .is_ok()
    );

    let mut moved = fixture_rule();
    moved.source.builder.sha = CommitSha::from_bytes([0; 20]);
    assert!(matches!(
        keys.check(&moved, token(), issued_at()).await,
        Err(ProofRefusal::Claim(AuthorityRefusal::Mismatch {
            field: "job_workflow_sha"
        }))
    ));
    assert!(matches!(
        keys.check(&fixture_rule(), token(), Timestamp::now()).await,
        Err(ProofRefusal::Token(TokenRefusal::Lifetime(_)))
    ));

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let rule = fixture_rule().rule.to_string();
    assert_eq!(logs.matches(&rule).count(), 2, "{logs}");
    assert!(logs.contains("claim=\"job_workflow_sha\""), "{logs}");
    for part in parts(token()) {
        assert!(!logs.contains(&part), "the log holds part of the token");
    }
}
