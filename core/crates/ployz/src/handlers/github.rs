//! `ployz github`: install the GitHub App so Services can build private
//! repositories, see what it grants, read a repository's files, and disconnect
//! it. Acts in Cloud as this device's sign-in or `PLOYZ_TOKEN`; needs no Server.

use super::catalog::{Approval::*, Runnable, cloud};
use std::time::{Duration, Instant};

use clap::{ArgMatches, Command};
use ployz_core::{RpcError, RpcErrorCode};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::account::in_cloud;
use super::login::open_browser;
use super::store::Next;
use super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};
use crate::cloud_account::{self, Credential};
use crate::cloud_login::LoginError;
use crate::ui::{Cell, Hint, Table, Tone};

/// How long `github connect` waits for the App to be installed.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_secs(3);

pub(crate) fn command() -> Command {
    Command::new("github")
        .about("Connect GitHub so Services can build your repositories")
        .subcommand(
            Command::new("connect")
                .about("Print the GitHub App install link and wait until it is installed")
                .long_about("Print the GitHub App install link and wait until GitHub reports the install. With --json, prints the link at once unless --wait is given. Public repositories need no install.")
                .arg(switch("wait", None).help("Wait for the install, also with --json")),
        )
        .subcommand(
            Command::new("ls")
                .about("List your GitHub installations and repositories, or one repository's branches")
                .arg(positional("repository", false).help("OWNER/REPO: list its branches")),
        )
        .subcommand(
            Command::new("tree")
                .about("List a repository's files")
                .arg(positional("repository", true).help("OWNER/REPO"))
                .arg(positional("path", false).help("Only files under this directory"))
                .arg(value("ref", None).help("Branch, tag or commit; the default branch if omitted")),
        )
        .subcommand(
            Command::new("cat")
                .about("Print one file of a repository")
                .arg(positional("repository", true).help("OWNER/REPO"))
                .arg(positional("path", true).help("The file, like Dockerfile or web/package.json"))
                .arg(value("ref", None).help("Branch, tag or commit; the default branch if omitted")),
        )
        .subcommand(
            Command::new("disconnect")
                .about("Forget one of your GitHub installations; uninstall the App on GitHub")
                .arg(
                    positional("installation", true)
                        .value_parser(clap::value_parser!(u64).range(1..))
                        .help("Installation ID, from ployz github ls"),
                ),
        )
}

pub(super) fn handler(path: &str) -> Option<Runnable> {
    Some(match path {
        "connect" => cloud(Never, connect),
        "ls" => cloud(Never, list),
        "tree" => cloud(Never, tree),
        "cat" => cloud(Never, cat),
        "disconnect" => cloud(Always, disconnect),
        _ => return None,
    })
}

/// The caller's GitHub connection, as Cloud reports it.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Connection {
    install_url: String,
    /// Whether the caller's GitHub account is linked; an install completes only then.
    linked: bool,
    /// Whether an installation grants at least one repository.
    ready: bool,
    installations: Vec<Installation>,
    repositories: Vec<Repository>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Installation {
    id: u64,
    account: String,
    account_type: String,
    repositories: u64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Repository {
    repository: String,
    private: bool,
    default_branch: String,
    installation: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct Branches {
    repository: String,
    access: String,
    default_branch: String,
    branches: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Tree {
    repository: String,
    #[serde(rename = "ref")]
    git_ref: String,
    paths: Vec<String>,
    /// Whether GitHub or Cloud cut the listing short.
    truncated: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct File {
    repository: String,
    #[serde(rename = "ref")]
    git_ref: String,
    path: String,
    size: u64,
    /// The text, or `None` for a binary or oversized file, which `note` explains.
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Disconnected {
    disconnected: Disconnection,
    uninstall_url: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Disconnection {
    id: u64,
    account: String,
}

async fn connection(credential: &Credential) -> Result<Connection, LoginError> {
    cloud_account::call(credential, Method::GET, "github", None).await
}

fn connect(root: &ArgMatches) -> Result<(), Error> {
    let wait = leaf_matches(root).get_flag("wait") || !crate::ui::json();
    let (before, after) = in_cloud(root, async |_, credential| {
        let before = connection(credential).await?;
        if !before.linked || !wait {
            return Ok((before, None));
        }
        crate::ui::note(format_args!(
            "Install the Ployz GitHub App: {}",
            before.install_url
        ));
        if crate::ui::interactive() {
            open_browser(&before.install_url);
        }
        crate::ui::note_inline(format_args!("Waiting for GitHub... "));
        let deadline = Instant::now() + INSTALL_TIMEOUT;
        while Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
            let now = connection(credential).await?;
            // A new installation, or new repositories granted to an existing one.
            if now.installations != before.installations || now.repositories != before.repositories
            {
                return Ok((before, Some(now)));
            }
        }
        Ok((before, None))
    })?;
    if !before.linked {
        return Err(Error::coded(
            RpcErrorCode::Unsupported,
            "GitHub reports installs by GitHub account, and yours isn't linked: sign in to Ployz Cloud with GitHub once, then rerun",
        )
        .hint(Hint::Next("ployz github connect".into())));
    }
    let Some(after) = after else {
        if wait {
            return Err(Error::detailed(
                RpcErrorCode::Unavailable,
                "GitHub hasn't reported the install yet",
                json!({ "url": before.install_url }),
            )
            .hint(Hint::Retry("ployz github connect --wait".into())));
        }
        let pending =
            json!({ "status": "pending", "url": before.install_url, "connection": before });
        return crate::ui::emit(&Next::new(
            &pending,
            Some("ployz github connect --wait".to_owned()),
        ));
    };
    crate::ui::note("done.");
    repositories(
        &Next::new(
            &after,
            Some("ployz service add NAME --repo OWNER/REPO".to_owned()),
        ),
        &after,
    )
}

fn list(root: &ArgMatches) -> Result<(), Error> {
    let Some(repository) = leaf_matches(root).get_one::<String>("repository") else {
        let listed = in_cloud(root, async |_, credential| connection(credential).await)?;
        let next = (!listed.ready).then(|| "ployz github connect".to_owned());
        repositories(&Next::new(&listed, next.clone()), &listed)?;
        if let Some(next) = next {
            crate::ui::hint(&Hint::Next(next));
        }
        return Ok(());
    };
    if !valid_repository(repository) {
        return Err(Error::usage(
            "Expected a repository as OWNER/REPO, like acme/web",
        ));
    }
    let path = format!("github/branches?repository={repository}");
    let branches: Branches = in_cloud(root, async |_, credential| {
        match cloud_account::call(credential, Method::GET, &path, None).await {
            // A GET's 404 reads as "no CLI surface"; the surface answering means no such repository.
            Err(LoginError::Unsupported(_)) if connection(credential).await.is_ok() => Ok(None),
            reply => found(reply),
        }
    })?
    .ok_or_else(|| not_found("No repository by that name that this Organization can read"))?;
    let mut table = Table::new(
        ["BRANCH", "DEFAULT"],
        format!("No branches in {} yet.", branches.repository),
    );
    for branch in &branches.branches {
        table.row([
            Cell::from(branch),
            if *branch == branches.default_branch {
                Cell::status("default", Tone::Good)
            } else {
                Cell::from("")
            },
        ]);
    }
    crate::ui::note(format_args!(
        "{} is {} to this Organization.",
        branches.repository, branches.access
    ));
    crate::ui::list(&branches, &table)
}

fn tree(root: &ArgMatches) -> Result<(), Error> {
    let tree: Tree = read_repository(root, "tree", "path")?;
    crate::ui::finish(&tree, || {
        if tree.paths.is_empty() {
            crate::ui::note(format_args!(
                "No files in {} at {}.",
                tree.repository, tree.git_ref
            ));
        }
        for path in &tree.paths {
            crate::ui::stream(path);
        }
        if tree.truncated {
            crate::ui::note("The listing is cut short: name a directory to see the rest.");
        }
    })?;
    Ok(())
}

fn cat(root: &ArgMatches) -> Result<(), Error> {
    let file: File = read_repository(root, "file", "path")?;
    crate::ui::finish(&file, || match &file.content {
        Some(content) => {
            let _ = std::io::Write::write_all(&mut anstream::stdout(), content.as_bytes());
        }
        None => crate::ui::note(file.note.as_deref().unwrap_or("This file isn't text.")),
    })?;
    Ok(())
}

/// Cloud's `github/<route>` reading of a repository, at `--ref` and the `path` argument.
fn read_repository<T: serde::de::DeserializeOwned>(
    root: &ArgMatches,
    route: &str,
    path: &str,
) -> Result<T, Error> {
    let matches = leaf_matches(root);
    let repository = matches
        .get_one::<String>("repository")
        .expect("repository is required");
    if !valid_repository(repository) {
        return Err(Error::usage(
            "Expected a repository as OWNER/REPO, like acme/web",
        ));
    }
    let mut query = reqwest::Url::parse("ployz:/").expect("a constant URL parses");
    query
        .query_pairs_mut()
        .append_pair("repository", repository);
    for (key, value) in [
        (path, matches.get_one::<String>(path)),
        ("ref", matches.get_one::<String>("ref")),
    ] {
        if let Some(value) = value {
            query.query_pairs_mut().append_pair(key, value);
        }
    }
    let path = format!("github/{route}?{}", query.query().unwrap_or_default());
    in_cloud(root, async |_, credential| {
        match cloud_account::call(credential, Method::GET, &path, None).await {
            // A GET's 404 reads as "no CLI surface"; the surface answering means no such repository.
            Err(LoginError::Unsupported(_)) if connection(credential).await.is_ok() => Ok(Err(
                not_found("No repository by that name that this Organization can read"),
            )),
            Err(LoginError::Status { body, .. }) if let Some(refused) = refusal(&body) => {
                Ok(Err(refused.into()))
            }
            reply => reply.map(Ok),
        }
    })?
}

/// Cloud's refusal of a call, as it sent it.
fn refusal(body: &str) -> Option<RpcError> {
    #[derive(Deserialize)]
    struct Refusal {
        error: RpcError,
    }
    serde_json::from_str::<Refusal>(body)
        .ok()
        .map(|refusal| refusal.error)
}

fn disconnect(root: &ArgMatches) -> Result<(), Error> {
    let id = *leaf_matches(root)
        .get_one::<u64>("installation")
        .expect("installation is required");
    let path = format!("github/{id}");
    let removed: Disconnected = in_cloud(root, async |_, credential| {
        found(cloud_account::call(credential, Method::DELETE, &path, None).await)
    })?
    .ok_or_else(|| not_found("No such GitHub installation of yours"))?;
    crate::ui::done(
        &removed,
        format_args!(
            "Disconnected the GitHub installation on {}.",
            removed.disconnected.account
        ),
    )?;
    crate::ui::note(format_args!(
        "Uninstall the App on GitHub to revoke its access: {}",
        removed.uninstall_url
    ));
    Ok(())
}

/// The repositories the App reaches, after a note per installation.
fn repositories<T: serde::Serialize>(value: &T, connection: &Connection) -> Result<(), Error> {
    for installation in &connection.installations {
        crate::ui::note(format_args!(
            "Installed on {} (installation {}, {} repositories).",
            installation.account, installation.id, installation.repositories
        ));
    }
    let mut table = Table::new(
        ["REPOSITORY", "VISIBILITY", "DEFAULT BRANCH"],
        "No repositories yet.",
    );
    for repository in &connection.repositories {
        table.row([
            Cell::from(repository.repository.to_string()),
            Cell::from(if repository.private {
                "private"
            } else {
                "public"
            }),
            Cell::from(repository.default_branch.to_string()),
        ]);
    }
    crate::ui::list(value, &table)
}

/// Cloud's 404 as `None`.
fn found<T>(reply: Result<T, LoginError>) -> Result<Option<T>, LoginError> {
    match reply {
        Ok(value) => Ok(Some(value)),
        Err(LoginError::Status { status: 404, .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn not_found(message: &'static str) -> Error {
    Error::not_found(message).hint(Hint::Next("ployz github ls".into()))
}

fn valid_repository(name: &str) -> bool {
    name.split_once('/').is_some_and(|(owner, repository)| {
        let fits = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        };
        fits(owner) && fits(repository) && name.len() <= 300
    })
}
