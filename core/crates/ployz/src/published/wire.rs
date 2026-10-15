//! The manifest as Ployz Cloud serves it at `/api/cli/manifest`. Kept apart from the
//! command model so a Cloud field can change without touching clap or the runners.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub(super) format: u32,
    pub(super) revision: String,
    pub(super) commands: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub(super) name: String,
    pub(super) path: Vec<String>,
    pub(super) about: String,
    #[serde(default)]
    pub(super) examples: Vec<String>,
    pub(super) kind: Kind,
    pub(super) gate: Gate,
    #[serde(default)]
    pub(super) removes: bool,
    pub(super) agents: bool,
    #[serde(default)]
    pub(super) typed: Option<Typed>,
    #[serde(default)]
    pub(super) positional: Vec<String>,
    pub(super) input: Value,
    pub(super) output: Value,
    pub(super) render: Render,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Kind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Gate {
    Never,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Typed {
    Organization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Render {
    Table,
    Record,
    Message,
}

/// What the cache file beside the config holds: the manifest as served, and the
/// Cloud it came from so a cache for one Cloud never answers for another.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Cached {
    pub(super) cloud: String,
    pub(super) manifest: Manifest,
}

/// One run's reply: `output` as the entry's output schema shapes it, and the
/// sentence a `message` render prints.
#[derive(Debug, Deserialize)]
pub(crate) struct Ran {
    pub(crate) output: Value,
    #[serde(default)]
    pub(crate) message: Option<String>,
}
