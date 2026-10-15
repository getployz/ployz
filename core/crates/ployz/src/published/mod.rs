//! Commands Ployz Cloud publishes as data. A cloudless Cluster never needs one, so
//! `ployz` compiles none of them: it reads Cloud's manifest, grafts each entry under
//! its noun path, and runs it by posting the arguments back. A compiled command keeps
//! its name when a published one collides.

mod graft;
mod run;
mod wire;

pub(crate) use graft::attach;
pub(crate) use run::{run, run_on};
pub(crate) use wire::Ran;

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::{RpcError, RpcErrorCode};
use reqwest::StatusCode;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::cloud_account::{self, Credential};
use crate::cloud_login::{self, CredentialStore, DEFAULT_CLOUD, LoginError};

/// The manifest one Cloud serves, as `ployz` uses it.
#[derive(Debug, Clone)]
pub(crate) struct Published {
    pub(crate) commands: Vec<PublishedCommand>,
    pub(crate) revision: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PublishedCommand {
    /// The MCP tool name; snake_case, never a compiled tool's.
    pub(crate) name: String,
    /// The CLI nouns and verb, `server upgrade-policy show`.
    pub(crate) path: Vec<String>,
    pub(crate) about: String,
    pub(crate) examples: Vec<String>,
    pub(crate) effect: Effect,
    /// Whether `ployz mcp` lists it.
    pub(crate) agents: bool,
    pub(crate) typed: Option<Typed>,
    pub(crate) args: Vec<PublishedArg>,
    /// The input JSON Schema as published; `ployz mcp` lists it unchanged.
    pub(crate) input: Value,
    pub(crate) output: Value,
    pub(crate) render: Render,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    Read,
    /// `removes`: it removes or revokes something, so hosts mark it destructive.
    Write {
        ask: bool,
        removes: bool,
    },
}

/// A name the user types before the command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Typed {
    Organization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Render {
    Table,
    Record,
    Message,
}

/// One input property as a CLI argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishedArg {
    pub(crate) name: String,
    pub(crate) kind: ArgKind,
    pub(crate) required: bool,
    pub(crate) positional: bool,
    pub(crate) help: Option<String>,
}

/// The flat schema subset a command line can carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArgKind {
    Text,
    Integer,
    Boolean,
    Choice(Vec<String>),
    Texts,
}

#[derive(Debug, Error)]
pub(crate) enum PublishError {
    /// The Cloud predates published commands: `/api/cli/manifest` is not there.
    #[error("Ployz Cloud at {cloud} publishes no commands")]
    Unsupported { cloud: String },
    /// The Cloud has the route but this Organization cannot use it.
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Refused(RpcError),
    #[error(transparent)]
    Cloud(#[from] LoginError),
    #[error("Ployz Cloud at {cloud} sent a manifest ployz cannot read: {reason}")]
    Malformed { cloud: String, reason: String },
    #[error("could not keep the published commands at {path}")]
    Cache {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not start a runtime")]
    Runtime(#[source] io::Error),
    /// Cloud answered `unknown_command`: the entry is gone from the manifest.
    #[error("Ployz Cloud no longer has the command ployz {path}")]
    Gone { path: String },
    /// The command asks for a typed name and there is no terminal to type it on.
    #[error("ployz {path} asks for the Organization's name on a terminal")]
    NeedsTerminal { path: String },
    /// The commands were read from one Cloud and the credential acts in another.
    #[error(
        "the published commands came from Ployz Cloud at {published}, but ployz acts in {acting}"
    )]
    OtherCloud { published: String, acting: String },
    #[error("Ployz Cloud at {cloud} did not list its commands within {budget:?}")]
    Slow { cloud: String, budget: Duration },
}

impl PublishError {
    pub(crate) fn code(&self) -> RpcErrorCode {
        match self {
            Self::Unsupported { .. } => RpcErrorCode::Unsupported,
            Self::Unavailable(_) => RpcErrorCode::Unavailable,
            Self::Refused(error) => error.code.clone(),
            Self::Cloud(error) => crate::failure::login_code(error),
            Self::Malformed { .. } | Self::Cache { .. } | Self::Runtime(_) => {
                RpcErrorCode::Internal
            }
            Self::Gone { .. } => RpcErrorCode::NotFound,
            Self::NeedsTerminal { .. } => RpcErrorCode::ConfirmationRequired,
            Self::OtherCloud { .. } => RpcErrorCode::Conflict,
            Self::Slow { .. } => RpcErrorCode::Unavailable,
        }
    }
}

/// Which Cloud publishes, and where its manifest is kept on this device.
#[derive(Debug, Clone)]
pub(crate) struct Setup {
    pub(crate) cloud: String,
    /// Whether anything names a Cloud: `PLOYZ_CLOUD_URL`, `PLOYZ_TOKEN`, or a sign-in.
    /// Without one, a command line that misses never costs a network round trip.
    pub(crate) configured: bool,
    config: PathBuf,
    dir: PathBuf,
}

/// How long a refresh may hold a command up, every request in it together.
pub(crate) const REFRESH_BUDGET: Duration = Duration::from_secs(2);

pub(crate) const UNKNOWN_COMMAND: &str = "unknown_command";
const FEATURE_UNAVAILABLE: &str = "feature_unavailable";

impl Setup {
    /// The Cloud the credential acts in, as [`cloud_account::named_cloud`] picks it from
    /// `PLOYZ_TOKEN`, `cloud` (`PLOYZ_CLOUD_URL`) and the sign-in beside `config`.
    pub(crate) fn beside(config: &Path, cloud: Option<String>) -> Self {
        let token = std::env::var_os(crate::cli::env::TOKEN).is_some_and(|token| !token.is_empty());
        let named = cloud_account::named_cloud(&CredentialStore::beside(config), token, cloud)
            .ok()
            .flatten();
        let configured = named.is_some() || token;
        Self {
            cloud: named.unwrap_or_else(|| crate::cloud_enroll::cloud_origin(DEFAULT_CLOUD)),
            configured,
            config: config.to_path_buf(),
            dir: config.with_file_name("published"),
        }
    }

    /// The credential for the Cloud, if the process has one.
    ///
    /// # Errors
    ///
    /// Returns a sign-in that exists but cannot be used; signed out is `None`.
    pub(crate) async fn credential(&self) -> Result<Option<Credential>, LoginError> {
        match cloud_account::from_env(&CredentialStore::beside(&self.config)).await {
            Ok(credential) => Ok(Some(credential)),
            Err(LoginError::SignedOut) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Refresh on a runtime of its own, for the parse path that has none yet. Fails
    /// are the caller's to ignore: the cache, or the compiled tree, still answers.
    ///
    /// # Errors
    ///
    /// Returns what [`Setup::refresh`] does. A credential that cannot be used counts as
    /// none.
    pub(crate) fn refresh_now(&self) -> Result<Published, PublishError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(PublishError::Runtime)?;
        runtime.block_on(self.refresh(async { self.credential().await.ok().flatten() }))
    }

    /// The setup the process environment names, as `cli::command()` reads it before
    /// any argument is parsed.
    pub(crate) fn from_env() -> Self {
        use crate::cli::env;
        let config = std::env::var(env::CONFIG)
            .ok()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "~/.config/ployz/config.yaml".to_owned());
        Self::beside(
            &crate::context::expand_home(Path::new(&config)),
            std::env::var(env::CLOUD_URL).ok(),
        )
    }

    /// Refuse to run a command read from this Cloud with a credential for another.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::OtherCloud`] when they differ.
    pub(crate) fn acts_with(&self, credential: &Credential) -> Result<(), PublishError> {
        if credential.cloud() == self.cloud {
            return Ok(());
        }
        Err(PublishError::OtherCloud {
            published: self.cloud.clone(),
            acting: credential.cloud().to_owned(),
        })
    }

    fn path(&self) -> PathBuf {
        let hash = Sha256::digest(self.cloud.as_bytes());
        self.dir.join(format!("{}.json", hex(&hash)))
    }

    /// The manifest kept from the last refresh, if any; never the network.
    pub(crate) fn cached(&self) -> Option<Published> {
        let bytes = std::fs::read(self.path()).ok()?;
        let cached: wire::Cached = serde_json::from_slice(&bytes).ok()?;
        (cached.cloud == self.cloud).then(|| Published::from_wire(cached.manifest))
    }

    fn keep(&self, manifest: &wire::Manifest) -> Result<(), PublishError> {
        let path = self.path();
        let failed = |source| PublishError::Cache {
            path: path.clone(),
            source,
        };
        std::fs::create_dir_all(&self.dir).map_err(failed)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.dir).map_err(failed)?;
        let cached = wire::Cached {
            cloud: self.cloud.clone(),
            manifest: manifest.clone(),
        };
        serde_json::to_writer_pretty(&mut file, &cached).map_err(|error| failed(error.into()))?;
        file.persist(&path).map_err(|error| failed(error.error))?;
        Ok(())
    }

    /// Forget one entry Cloud said it no longer has, so `--help` stops showing it.
    pub(crate) fn evict(&self, name: &str) {
        let Ok(bytes) = std::fs::read(self.path()) else {
            return;
        };
        let Ok(mut cached) = serde_json::from_slice::<wire::Cached>(&bytes) else {
            return;
        };
        cached.manifest.commands.retain(|entry| entry.name != name);
        let _ = self.keep(&cached.manifest);
    }

    /// Fetch the manifest, within [`REFRESH_BUDGET`], and keep it. Unsigned first,
    /// then with `credential` when Cloud asks for one and `credential` is for this
    /// Cloud. `credential` is resolved only then, inside the budget, since resolving one
    /// can mean asking Cloud about a pending sign-in. A cached revision is sent so an
    /// unchanged manifest costs no body.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::Unsupported`] for a Cloud without the route,
    /// [`PublishError::Unavailable`] when Cloud refuses the feature,
    /// [`PublishError::Slow`] past the budget, and the transport or cache failure
    /// otherwise.
    pub(crate) async fn refresh(
        &self,
        credential: impl Future<Output = Option<Credential>>,
    ) -> Result<Published, PublishError> {
        tokio::time::timeout(REFRESH_BUDGET, self.fetch(credential))
            .await
            .unwrap_or_else(|_| {
                Err(PublishError::Slow {
                    cloud: self.cloud.clone(),
                    budget: REFRESH_BUDGET,
                })
            })
    }

    async fn fetch(
        &self,
        credential: impl Future<Output = Option<Credential>>,
    ) -> Result<Published, PublishError> {
        let cached = self.cached();
        let url = format!("{}/api/cli/manifest", self.cloud);
        let client = cloud_login::http()?;
        let send = |credential: Option<&Credential>| {
            let mut request = client.get(&url);
            if let Some(credential) = credential {
                request = request.bearer_auth(credential.bearer());
            }
            if let Some(cached) = &cached {
                request = request.header("if-none-match", format!("\"{}\"", cached.revision));
            }
            request.send()
        };
        let mut response = send(None)
            .await
            .map_err(|error| cloud_login::unreachable(&self.cloud, error))?;
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) && let Some(credential) = credential
            .await
            .filter(|credential| credential.cloud() == self.cloud)
        {
            response = send(Some(&credential))
                .await
                .map_err(|error| cloud_login::unreachable(&self.cloud, error))?;
        }
        let status = response.status();
        cloud_account::notice_revision(&response);
        let bytes = response
            .bytes()
            .await
            .map_err(|error| cloud_login::unreachable(&self.cloud, error))?;
        if status == StatusCode::NOT_MODIFIED
            && let Some(cached) = cached
        {
            return Ok(cached);
        }
        if !status.is_success() {
            if let Some(refused) = cloud_account::refusal(&bytes) {
                return Err(match refused.code.as_str() {
                    FEATURE_UNAVAILABLE => PublishError::Unavailable(refused.message),
                    _ => PublishError::Refused(refused),
                });
            }
            return Err(match status {
                StatusCode::NOT_FOUND => PublishError::Unsupported {
                    cloud: self.cloud.clone(),
                },
                StatusCode::UNAUTHORIZED => LoginError::TokenRefused.into(),
                status => LoginError::Status {
                    status: status.as_u16(),
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                }
                .into(),
            });
        }
        let manifest: wire::Manifest =
            serde_json::from_slice(&bytes).map_err(|error| PublishError::Malformed {
                cloud: self.cloud.clone(),
                reason: error.to_string(),
            })?;
        if manifest.format != wire::FORMAT {
            return Err(PublishError::Malformed {
                cloud: self.cloud.clone(),
                reason: format!(
                    "manifest format {} is not {}",
                    manifest.format,
                    wire::FORMAT
                ),
            });
        }
        self.keep(&manifest)?;
        Ok(Published::from_wire(manifest))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Published {
    /// The manifest from JSON text, as a test holds it.
    #[cfg(test)]
    pub(crate) fn parse(json: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(json).map(Self::from_wire)
    }

    fn from_wire(manifest: wire::Manifest) -> Self {
        let compiled: BTreeSet<&str> = ployz_store::tools::catalog()
            .iter()
            .map(|def| def.name)
            .collect();
        let mut seen = BTreeSet::new();
        let commands = manifest
            .commands
            .into_iter()
            .filter(|entry| !compiled.contains(entry.name.as_str()))
            .filter_map(PublishedCommand::from_wire)
            .filter(|command| seen.insert(command.name.clone()))
            .collect();
        Self {
            commands,
            revision: manifest.revision,
        }
    }

    pub(crate) fn find(&self, path: &str) -> Option<&PublishedCommand> {
        self.commands.iter().find(|command| command.path() == path)
    }
}

impl PublishedCommand {
    /// The noun path as `dispatch` names it, `server upgrade-policy show`.
    pub(crate) fn path(&self) -> String {
        self.path.join(" ")
    }

    pub(crate) fn asks(&self) -> bool {
        matches!(self.effect, Effect::Write { ask: true, .. })
    }

    pub(crate) fn removes(&self) -> bool {
        matches!(self.effect, Effect::Write { removes: true, .. })
    }

    fn from_wire(entry: wire::Entry) -> Option<Self> {
        if !tool_name(&entry.name)
            || entry.path.is_empty()
            || !entry.path.iter().all(|segment| noun(segment))
        {
            return None;
        }
        let args = args(&entry.input, &entry.positional)?;
        Some(Self {
            name: entry.name,
            path: entry.path,
            about: entry.about,
            examples: entry.examples,
            effect: match entry.kind {
                wire::Kind::Read => Effect::Read,
                wire::Kind::Write => Effect::Write {
                    ask: entry.gate == wire::Gate::Ask,
                    removes: entry.removes,
                },
            },
            agents: entry.agents,
            typed: entry.typed.map(|typed| match typed {
                wire::Typed::Organization => Typed::Organization,
            }),
            args,
            input: entry.input,
            output: entry.output,
            render: match entry.render {
                wire::Render::Table => Render::Table,
                wire::Render::Record => Render::Record,
                wire::Render::Message => Render::Message,
            },
        })
    }
}

fn tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|char| char.is_ascii_lowercase() || char.is_ascii_digit() || char == '_')
}

fn noun(segment: &str) -> bool {
    let mut chars = segment.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|char| char.is_ascii_lowercase() || char.is_ascii_digit() || char == '-')
}

/// The input schema's properties as arguments, in schema order. `None` when a property
/// is outside the flat subset or a positional names no property.
fn args(input: &Value, positional: &[String]) -> Option<Vec<PublishedArg>> {
    let properties = input.get("properties")?.as_object()?;
    let required: BTreeSet<&str> = input
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !positional.iter().all(|name| properties.contains_key(name)) {
        return None;
    }
    let mut args = Vec::new();
    for (name, schema) in properties {
        if !noun(name) {
            return None;
        }
        args.push(PublishedArg {
            kind: kind(schema)?,
            required: required.contains(name.as_str()),
            positional: positional.contains(name),
            help: schema
                .get("description")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            name: name.clone(),
        });
    }
    let (mut positionals, named): (Vec<_>, Vec<_>) =
        args.into_iter().partition(|arg| arg.positional);
    positionals.sort_by_key(|arg| positional.iter().position(|name| *name == arg.name));
    positionals.extend(named);
    Some(positionals)
}

fn kind(schema: &Value) -> Option<ArgKind> {
    let choices = schema.get("enum").and_then(Value::as_array);
    match schema.get("type")?.as_str()? {
        "string" => Some(match choices {
            Some(choices) => ArgKind::Choice(
                choices
                    .iter()
                    .map(|choice| choice.as_str().map(ToOwned::to_owned))
                    .collect::<Option<_>>()?,
            ),
            None => ArgKind::Text,
        }),
        "integer" => Some(ArgKind::Integer),
        "boolean" => Some(ArgKind::Boolean),
        "array" => {
            (schema.get("items")?.get("type")?.as_str()? == "string").then_some(ArgKind::Texts)
        }
        _ => None,
    }
}

#[cfg(test)]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/published-manifest.json");

    fn manifest() -> wire::Manifest {
        serde_json::from_slice(FIXTURE).unwrap()
    }

    fn setup(home: &Path, cloud: &str) -> Setup {
        Setup::beside(&home.join("config.yaml"), Some(cloud.to_owned()))
    }

    fn names(published: &Published) -> Vec<&str> {
        published
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect()
    }

    #[test]
    fn a_kept_manifest_reads_back_with_every_entry_and_its_revision() {
        let home = tempfile::tempdir().unwrap();
        let setup = setup(home.path(), "https://cloud.test");
        assert!(setup.cached().is_none(), "nothing before the first keep");
        let manifest = manifest();
        setup.keep(&manifest).unwrap();
        let cached = setup.cached().expect("the kept manifest");
        assert_eq!(cached.revision, manifest.revision);
        assert_eq!(
            names(&cached),
            manifest
                .commands
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_cache_file_written_for_another_cloud_never_answers() {
        let home = tempfile::tempdir().unwrap();
        let theirs = setup(home.path(), "https://theirs.test");
        theirs.keep(&manifest()).unwrap();
        let ours = setup(home.path(), "https://ours.test");
        std::fs::copy(theirs.path(), ours.path()).unwrap();
        assert!(
            ours.cached().is_none(),
            "a copied cache names the Cloud it came from"
        );
    }

    #[test]
    fn evict_drops_one_entry_and_keeps_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let setup = setup(home.path(), "https://cloud.test");
        let manifest = manifest();
        setup.keep(&manifest).unwrap();
        let gone = manifest.commands[0].name.clone();
        setup.evict(&gone);
        let cached = setup.cached().unwrap();
        assert!(!names(&cached).contains(&gone.as_str()));
        assert_eq!(cached.commands.len(), manifest.commands.len() - 1);
        assert_eq!(
            cached.revision, manifest.revision,
            "eviction is not a refresh"
        );
    }

    /// A Cloud that refuses every request unsigned, and the headers of each it saw.
    fn refusing_cloud() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (saw, seen) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line.to_ascii_lowercase());
                }
                let _ = saw.send(headers);
                let body = "{}";
                write!(
                    stream,
                    "HTTP/1.1 401 X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    #[tokio::test]
    async fn a_credential_for_another_cloud_is_never_sent_and_never_runs() {
        let home = tempfile::tempdir().unwrap();
        let stored = serde_json::json!({
            "state": "signed_in",
            "cloud": "http://signed-in.test",
            "token": "session-token",
            "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
            "organization": { "id": "o1", "slug": "acme" },
        });
        std::fs::write(home.path().join("cloud.json"), stored.to_string()).unwrap();
        let store = CredentialStore::beside(&home.path().join("config.yaml"));
        let device = || async { cloud_account::credential(&store, None, None).await.ok() };
        let (url, seen) = refusing_cloud();
        let elsewhere = setup(tempfile::tempdir().unwrap().path(), &url);

        assert!(elsewhere.refresh(device()).await.is_err());
        let requests: Vec<String> = seen.try_iter().collect();
        assert_eq!(requests.len(), 1, "no signed retry: {requests:?}");
        assert!(!requests[0].contains("authorization:"), "{}", requests[0]);
        let device = device().await.unwrap();
        assert!(matches!(
            elsewhere.acts_with(&device),
            Err(PublishError::OtherCloud { .. })
        ));
    }

    #[test]
    fn evict_on_a_missing_cache_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let setup = setup(home.path(), "https://cloud.test");
        setup.evict("anything");
        assert!(!setup.dir.exists());
    }
}
