//! Commands Ployz Cloud publishes as data. A cloudless Cluster never needs one, so
//! `ployz` compiles none of them: it reads Cloud's manifest, grafts each entry under
//! its noun path, and runs it by posting the arguments back. A compiled command keeps
//! its name when a published one collides.

mod graft;
mod wire;

pub(crate) use graft::attach;

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
    pub(crate) cloud: String,
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
    Write { ask: bool },
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
    /// Cloud answered `unknown_command`: the entry is gone from the manifest.
    #[error("Ployz Cloud no longer has the command ployz {path}")]
    Gone { path: String },
    /// The command asks for a typed name and there is no terminal to type it on.
    #[error("ployz {path} asks for the Organization's name on a terminal")]
    NeedsTerminal { path: String },
}

impl PublishError {
    pub(crate) fn code(&self) -> RpcErrorCode {
        match self {
            Self::Unsupported { .. } => RpcErrorCode::Unsupported,
            Self::Unavailable(_) => RpcErrorCode::Unavailable,
            Self::Refused(error) => error.code.clone(),
            Self::Cloud(error) => crate::failure::login_code(error),
            Self::Malformed { .. } | Self::Cache { .. } => RpcErrorCode::Internal,
            Self::Gone { .. } => RpcErrorCode::NotFound,
            Self::NeedsTerminal { .. } => RpcErrorCode::ConfirmationRequired,
        }
    }
}

/// Which Cloud publishes, and where its manifest is kept on this device.
#[derive(Debug, Clone)]
pub(crate) struct Setup {
    pub(crate) cloud: String,
    dir: PathBuf,
}

/// How long a refresh may hold a command up.
pub(crate) const REFRESH_BUDGET: Duration = Duration::from_secs(2);

const UNKNOWN_COMMAND: &str = "unknown_command";
const FEATURE_UNAVAILABLE: &str = "feature_unavailable";

impl Setup {
    /// `PLOYZ_CLOUD_URL`, else the signed-in Cloud beside `config`, else the default.
    pub(crate) fn beside(config: &Path, cloud: Option<String>) -> Self {
        let cloud = cloud
            .filter(|cloud| !cloud.trim().is_empty())
            .or_else(|| CredentialStore::beside(config).cloud().ok().flatten())
            .unwrap_or_else(|| DEFAULT_CLOUD.to_owned());
        Self {
            cloud: crate::cloud_enroll::cloud_origin(&cloud),
            dir: config.with_file_name("published"),
        }
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

    fn path(&self) -> PathBuf {
        let hash = Sha256::digest(self.cloud.as_bytes());
        self.dir.join(format!("{}.json", hex(&hash)))
    }

    /// The manifest kept from the last refresh, if any; never the network.
    pub(crate) fn cached(&self) -> Option<Published> {
        let bytes = std::fs::read(self.path()).ok()?;
        let cached: wire::Cached = serde_json::from_slice(&bytes).ok()?;
        (cached.cloud == self.cloud).then(|| Published::from_wire(&self.cloud, cached.manifest))
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
    /// then with `credential` when Cloud asks for one. A cached revision is sent so an
    /// unchanged manifest costs no body.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::Unsupported`] for a Cloud without the route,
    /// [`PublishError::Unavailable`] when Cloud refuses the feature, and the transport
    /// or cache failure otherwise.
    pub(crate) async fn refresh(
        &self,
        credential: Option<&Credential>,
    ) -> Result<Published, PublishError> {
        let cached = self.cached();
        let url = format!("{}/api/cli/manifest", self.cloud);
        let client = cloud_login::http()?;
        let send = |credential: Option<&Credential>| {
            let mut request = client.get(&url).timeout(REFRESH_BUDGET);
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
        {
            response = send(Some(credential))
                .await
                .map_err(|error| cloud_login::unreachable(&self.cloud, error))?;
        }
        let status = response.status();
        cloud_account::notice_revision(response.headers());
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
        Ok(Published::from_wire(&self.cloud, manifest))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Published {
    /// The manifest from JSON text, as a test or the cache holds it.
    ///
    /// # Errors
    ///
    /// Returns the JSON decoding error.
    pub(crate) fn parse(cloud: &str, json: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(json).map(|manifest| Self::from_wire(cloud, manifest))
    }

    fn from_wire(cloud: &str, manifest: wire::Manifest) -> Self {
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
            cloud: cloud.to_owned(),
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
        self.effect == Effect::Write { ask: true }
    }

    fn from_wire(entry: wire::Entry) -> Option<Self> {
        if !tool_name(&entry.name) || entry.path.is_empty() || !entry.path.iter().all(noun) {
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

fn noun(segment: &String) -> bool {
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
