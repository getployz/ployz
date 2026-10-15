//! Tools that read GitHub through Cloud: a repository's files, and which
//! repositories Ployz may read.

use std::collections::BTreeMap;

use ployz_core::DockerfileFacts;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::environment::{GithubAnswer, REPOSITORY};
use super::machine::{Cx, Effect, Stop, Tool};
use super::refusal::{Call, Refusal};
use super::step::{Ask, CloudQuery};
use crate::RepositoryName;
use crate::github::{
    GithubFileBody, GithubFileQuery, GithubFileView, GithubTreeQuery, GithubTreeView,
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GithubLs {
    /// GitHub repository owner/name.
    #[schemars(with = "String", pattern(REPOSITORY))]
    repository: RepositoryName,
    /// A directory to list, or a file to read. Omit for the top.
    #[serde(default)]
    path: Option<String>,
    /// Branch, tag or commit. Default: the repository's default branch.
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    /// Only paths matching this glob, like *.rs or src/**.
    #[serde(default, rename = "match")]
    glob: Option<String>,
}

impl Tool for GithubLs {
    const NAME: &'static str = "github_ls";
    const DESCRIPTION: &'static str =
        "List a connected GitHub repository's files at a path, or read one file (at most 64 KB).";
    const EFFECT: Effect = Effect::Read;

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
            git_ref: Some(tree.git_ref.clone()),
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
        .with_next(Call::new(GithubLs::NAME, input));
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
    const DESCRIPTION: &'static str = "Whether GitHub is connected, which repositories Ployz can read, and the page that connects more.";
    const EFFECT: Effect = Effect::Read;

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
