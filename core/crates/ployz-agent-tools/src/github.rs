//! Tools that read GitHub through Cloud: a repository's files, and which
//! repositories Ployz may read.

use std::collections::BTreeMap;

use ployz_core::DockerfileFacts;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::ToolId;
use super::cloud::GithubAnswer;
use super::machine::{Cx, Effect, Reach, Stop, Tool};
use super::refusal::{Call, Refusal};
use super::schema::REPOSITORY;
use super::step::{Ask, CloudQuery};
use ployz_store::RepositoryName;
use ployz_store::{
    GithubFileBody, GithubFileQuery, GithubFileView, GithubTreeQuery, GithubTreeView,
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GithubLs {
    /// GitHub repository, e.g. `acme/api`.
    #[schemars(with = "String", pattern(REPOSITORY))]
    repository: RepositoryName,
    /// Directory to list or file to read, e.g. `src` or `Dockerfile`. Omit for the repository's root.
    #[serde(default)]
    path: Option<String>,
    /// Branch, tag or commit, e.g. `main`. Omit for the repository's default branch.
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    /// Glob, e.g. `*.rs` or `src/**`. Lists every matching file under path and reads none.
    #[serde(default, rename = "match")]
    glob: Option<String>,
}

impl Tool for GithubLs {
    const NAME: &'static str = "github_ls";
    const TITLE: &'static str = "List or read GitHub repository files";
    const DESCRIPTION: &'static str = "List the files and directories at a path in a GitHub repository Ployz can read, or read one text file up to 64 KB. A larger or binary file returns a note instead, and a long listing is cut short and says so. A Dockerfile also returns its exposed ports and its ENV and ARG names.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly { local_next: None };
    // Cloud answers from GitHub's API (dashboard/src/modules/github/github-cli.server.ts,
    // repositoryTree and repositoryFile).
    const OPEN_WORLD: bool = true;

    fn example() -> Value {
        json!({ "repository": "acme/api", "path": "src" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let tree = GithubTreeQuery {
            repository: self.repository.clone(),
            path: self.path.clone(),
            git_ref: self.git_ref.clone(),
            glob: self.glob.clone(),
        };
        let mut answers = cx
            .read(vec![Ask::Cloud(CloudQuery::GithubTree(tree))])?
            .into_iter();
        let tree = cx.cloud::<GithubTreeView>(answers.next())?;
        let path = self
            .path
            .as_deref()
            .map(|path| path.trim_matches('/'))
            .filter(|path| !path.is_empty());
        if path.is_some() && self.glob.is_none() && tree.paths.is_empty() && !tree.truncated {
            return Err(missing(&self.repository, path, &tree.git_ref).into());
        }
        let is_file = path.is_some_and(|path| tree.paths.iter().any(|one| one == path));
        let Some(file) = path.filter(|_| is_file && self.glob.is_none()) else {
            return Ok(listing(&tree, path, self.glob.is_some()));
        };
        let query = GithubFileQuery {
            repository: self.repository,
            path: file.to_owned(),
            git_ref: Some(tree.git_ref),
        };
        let mut answers = cx
            .read(vec![Ask::Cloud(CloudQuery::GithubFile(query))])?
            .into_iter();
        let file = cx.cloud::<GithubFileView>(answers.next())?;
        let (content, note) = match file.body {
            GithubFileBody::Text { content } => (Some(content), None),
            GithubFileBody::Withheld { note } => (None, Some(note)),
        };
        let facts = content
            .as_deref()
            .filter(|_| is_dockerfile(&file.path))
            .map(DockerfileFacts::parse);
        let untrusted = match &facts {
            Some(facts) => {
                json!({ "content": content, "dockerfile": { "env": facts.env, "args": facts.args } })
            }
            None => json!({ "content": content }),
        };
        Ok(json!({
            "repository": file.repository,
            "ref": file.git_ref,
            "path": file.path,
            "untrusted": untrusted,
            "note": note,
            "dockerfile": facts.map(|facts| json!({ "ports": facts.ports })),
        }))
    }
}

/// `Dockerfile`, `Dockerfile.cloud` or `cloud.Dockerfile`, in any case.
fn is_dockerfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name == "dockerfile" || name.starts_with("dockerfile.") || name.ends_with(".dockerfile")
}

/// Nothing at `path`: list its parent instead.
fn missing(repository: &RepositoryName, path: Option<&str>, git_ref: &str) -> Refusal {
    let parent = path
        .and_then(|path| path.rsplit_once('/'))
        .map(|(parent, _)| parent);
    let input = match parent {
        Some(parent) => json!({ "repository": repository, "ref": git_ref, "path": parent }),
        None => json!({ "repository": repository, "ref": git_ref }),
    };
    let mut refusal = Refusal::not_found("No file or directory at that path")
        .with_next(Call::to(ToolId::GithubLs, input));
    refusal.field = Some("path".to_owned());
    refusal
}

/// A listing: with a glob every matching file, else what sits directly under
/// `path`, a directory standing for everything below it.
fn listing(tree: &GithubTreeView, path: Option<&str>, globbed: bool) -> Value {
    let prefix = path
        .filter(|path| !path.is_empty())
        .map(|path| format!("{path}/"));
    let mut entries = BTreeMap::new();
    for one in &tree.paths {
        let rest = match &prefix {
            Some(prefix) => match one.strip_prefix(prefix.as_str()) {
                Some(rest) => rest,
                None => continue,
            },
            None => one.as_str(),
        };
        let (name, kind) = match rest.split_once('/') {
            Some((directory, _)) if !globbed => (directory, "directory"),
            _ => (rest, "file"),
        };
        let full = prefix
            .as_deref()
            .map_or_else(|| name.to_owned(), |prefix| format!("{prefix}{name}"));
        entries.entry(full).or_insert(kind);
    }
    let entries = entries
        .into_iter()
        .map(|(path, kind)| json!({ "path": path, "kind": kind }))
        .collect::<Vec<_>>();
    let note = match (tree.truncated, entries.is_empty(), globbed) {
        (true, ..) => Some("GitHub cut this listing short; narrow it with path or match"),
        (false, true, true) => {
            Some("No file matches; the path may not exist or the glob matches nothing")
        }
        (false, true, false) => Some("This repository has no files at that ref"),
        (false, false, _) => None,
    };
    json!({
        "repository": tree.repository,
        "ref": tree.git_ref,
        "untrusted": { "entries": entries },
        "note": note,
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GithubConnect {}

impl Tool for GithubConnect {
    const NAME: &'static str = "github_connect";
    const TITLE: &'static str = "Show the GitHub connection";
    const DESCRIPTION: &'static str = "Show whether GitHub is connected, the repositories Ployz can read, and the page where a member connects more.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly { local_next: None };

    fn example() -> Value {
        json!({})
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let mut answers = cx
            .read(vec![Ask::Cloud(CloudQuery::GithubConnection)])?
            .into_iter();
        let github = cx.cloud::<GithubAnswer>(answers.next())?;
        Ok(json!({
            "connected": github.connected,
            "repositories": github.repositories,
            "connect_at": github.connect_at,
        }))
    }
}
