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

/// GitHub's compare as the ancestry read sees it: [`HEAD`] descends from [`BASE`], is an
/// ancestor of [`AHEAD_OF_MAIN`], and shares only history with [`FORKED`]. A tag `main`
/// points at [`FORKED`], so a compare against the name would find it on the branch.
/// Another commit is not found.
async fn compare(
    Path((id, basehead)): Path<(u64, String)>,
    headers: HeaderMap,
    query: axum::extract::RawQuery,
) -> Response {
    let authorized = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(&format!("Bearer {READER}"));
    if !authorized || id != REPOSITORY || query.0.as_deref() != Some("per_page=1") {
        return AxumStatus::NOT_FOUND.into_response();
    }
    let Some((base, head)) = basehead.split_once("...") else {
        return AxumStatus::NOT_FOUND.into_response();
    };
    let status = match (base, head) {
        (HEAD, HEAD) | (FORKED, "main") => "identical",
        (BASE, HEAD) => "ahead",
        (AHEAD_OF_MAIN, HEAD) => "behind",
        (FORKED, HEAD) => "diverged",
        (LIAR, HEAD) => {
            return Json(json!({"status": "ahead", "base_commit": {"sha": HEAD}})).into_response();
        }
        _ => return AxumStatus::NOT_FOUND.into_response(),
    };
    Json(json!({"status": status, "ahead_by": 1, "base_commit": {"sha": base}})).into_response()
}

const BASE: &str = "1111111111111111111111111111111111111111";
const AHEAD_OF_MAIN: &str = "2222222222222222222222222222222222222222";
const FORKED: &str = "3333333333333333333333333333333333333333";
const LIAR: &str = "4444444444444444444444444444444444444444";

async fn api() -> GithubApi {
    let app = Router::new()
        .route("/repositories/{id}/git/ref/heads/{*branch}", get(github))
        .route("/repositories/{id}/compare/{*basehead}", get(compare));
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

#[tokio::test]
async fn compare_reads_how_the_head_stands_to_a_commit() {
    let api = api().await;
    let reader = token(READER);
    let head = CommitSha::parse(HEAD).unwrap();
    for (commit, ancestry) in [
        (HEAD, Ancestry::Identical),
        (BASE, Ancestry::Ahead),
        (AHEAD_OF_MAIN, Ancestry::Behind),
        // On the branch only by a tag that shares its name.
        (FORKED, Ancestry::Diverged),
    ] {
        let read = api
            .compare(
                REPOSITORY,
                CommitSha::parse(commit).unwrap(),
                head,
                Some(&reader),
            )
            .await
            .unwrap();
        assert_eq!(read, ancestry, "{commit}");
        assert_eq!(
            read.on_branch(),
            matches!(ancestry, Ancestry::Identical | Ancestry::Ahead)
        );
    }
}

#[tokio::test]
async fn compare_refuses_an_answer_about_another_commit_or_none() {
    let api = api().await;
    let compare = |commit: &str, credential: Option<GithubToken>| {
        let api = &api;
        let commit = CommitSha::parse(commit).unwrap();
        async move {
            api.compare(
                REPOSITORY,
                commit,
                CommitSha::parse(HEAD).unwrap(),
                credential.as_ref(),
            )
            .await
        }
    };
    assert!(matches!(
        compare(LIAR, Some(token(READER))).await,
        Err(HeadError::Malformed)
    ));
    assert!(matches!(
        compare(&"5".repeat(40), Some(token(READER))).await,
        Err(HeadError::Unreadable { .. })
    ));
    assert!(matches!(
        compare(BASE, None).await,
        Err(HeadError::Unreadable { .. })
    ));
}

#[test]
fn a_compare_names_two_commits_and_no_ref() {
    let api = GithubApi::with(reqwest::Client::new(), Url::parse(API).unwrap());
    assert_eq!(
        api.compare_url(
            7,
            CommitSha::parse(BASE).unwrap(),
            CommitSha::parse(HEAD).unwrap()
        )
        .as_str(),
        format!("https://api.github.com/repositories/7/compare/{BASE}...{HEAD}")
    );
}
