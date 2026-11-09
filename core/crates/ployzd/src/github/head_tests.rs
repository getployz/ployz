use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode as AxumStatus},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;

use super::*;

const REPOSITORY: u64 = 1_450_985_696;
const HEAD: &str = "54dfcf4a46870fa09f5d5d6c965dd705c1a24e5c";
const READER: &str = "ghs_reader";

/// GitHub as the head read sees it: a private repository `REPOSITORY` whose branches
/// answer only to `READER`, and whose `limited` branch answers 403 as a rate limit does.
async fn github(Path((id, branch)): Path<(u64, String)>, headers: HeaderMap) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    match authorization {
        Some("Bearer ghs_expired") => return AxumStatus::UNAUTHORIZED.into_response(),
        Some(value) if value == format!("Bearer {READER}") => {}
        _ => return AxumStatus::NOT_FOUND.into_response(),
    }
    if id != REPOSITORY {
        return AxumStatus::NOT_FOUND.into_response();
    }
    match branch.as_str() {
        "main" | "feature/x" => Json(json!({
            "ref": format!("refs/heads/{branch}"),
            "object": {"type": "commit", "sha": HEAD, "url": "ignored"}
        }))
        .into_response(),
        "liar" => Json(json!({
            "ref": "refs/heads/main",
            "object": {"type": "commit", "sha": HEAD}
        }))
        .into_response(),
        "limited" => AxumStatus::FORBIDDEN.into_response(),
        _ => AxumStatus::NOT_FOUND.into_response(),
    }
}

async fn api() -> GithubApi {
    let app = Router::new().route("/repositories/{id}/git/ref/heads/{*branch}", get(github));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    GithubApi::with(
        reqwest::Client::new(),
        Url::parse(&format!("http://{address}")).unwrap(),
    )
}

fn token(text: &str) -> GithubToken {
    GithubToken::new(text.to_owned())
}

#[tokio::test]
async fn a_credential_that_reads_the_repository_reads_its_head() {
    let api = api().await;
    let reader = token(READER);
    for branch in ["main", "feature/x"] {
        assert_eq!(
            api.branch_head(REPOSITORY, branch, Some(&reader))
                .await
                .unwrap(),
            CommitSha::parse(HEAD).unwrap(),
            "{branch}"
        );
    }
}

#[tokio::test]
async fn a_credential_that_cannot_see_the_repository_reads_nothing() {
    let api = api().await;
    for credential in [
        None,
        Some(token("ghs_other_installation")),
        Some(token("ghs_expired")),
    ] {
        assert!(matches!(
            api.branch_head(REPOSITORY, "main", credential.as_ref())
                .await,
            Err(HeadError::Unreadable { .. })
        ));
    }
    assert!(matches!(
        api.branch_head(REPOSITORY, "gone", Some(&token(READER)))
            .await,
        Err(HeadError::Unreadable { .. })
    ));
}

#[tokio::test]
async fn the_answer_must_be_about_the_branch_asked_for() {
    assert!(matches!(
        api()
            .await
            .branch_head(REPOSITORY, "liar", Some(&token(READER)))
            .await,
        Err(HeadError::Malformed)
    ));
}

#[tokio::test]
async fn a_rate_limit_is_not_mistaken_for_an_unreadable_branch() {
    assert!(matches!(
        api()
            .await
            .branch_head(REPOSITORY, "limited", Some(&token(READER)))
            .await,
        Err(HeadError::Status {
            status: StatusCode::FORBIDDEN
        })
    ));
}

#[tokio::test]
async fn a_name_git_does_not_allow_is_never_asked_for() {
    let api = api().await;
    for branch in ["", "a//b", "../main", "main/.", "/main"] {
        assert!(matches!(
            api.branch_head(REPOSITORY, branch, None).await,
            Err(HeadError::Branch { .. })
        ));
    }
}

#[test]
fn a_branch_name_is_escaped_into_path_segments() {
    let api = GithubApi::with(reqwest::Client::new(), Url::parse(API).unwrap());
    assert_eq!(
        api.ref_url(7, "feature/a b?#").unwrap().as_str(),
        "https://api.github.com/repositories/7/git/ref/heads/feature/a%20b%3F%23"
    );
}

#[test]
fn a_token_never_prints() {
    let reader = token(READER);
    assert_eq!(format!("{reader:?}"), "GithubToken(..)");
}
