mod machine;
mod observation;
mod selector;
mod spec;

pub use machine::*;
pub use observation::*;
pub use selector::*;
pub use spec::*;

use serde::{Deserialize, Serialize};

use crate::MachineId;

pub const CADDY_VERIFY_PATH: &str = "/.ployz-verify";

/// A name lookup result. Duplicate names are a normal observable state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "match", content = "values")]
pub enum NameMatches<T> {
    None,
    One(T),
    Ambiguous(Vec<T>),
}

impl<T> NameMatches<T> {
    #[must_use]
    pub fn from_matches(mut matches: Vec<T>) -> Self {
        match matches.len() {
            0 => Self::None,
            1 => Self::One(matches.pop().expect("length checked")),
            _ => Self::Ambiguous(matches),
        }
    }
}

/// A successful response from one fan-out target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineSuccess<T> {
    pub machine_id: MachineId,
    pub value: T,
}

/// A typed failure returned for one fan-out target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineFailure<E> {
    pub machine_id: MachineId,
    pub error: E,
}

/// All outcomes observed during a fan-out operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PartialResult<T, E> {
    #[serde(default)]
    pub successes: Vec<MachineSuccess<T>>,
    #[serde(default)]
    pub failures: Vec<MachineFailure<E>>,
    /// Targets selected by the entry Machine that produced no terminal response.
    #[serde(default)]
    pub omissions: Vec<MachineId>,
}

impl<T, E> PartialResult<T, E> {
    #[must_use]
    pub fn all_targets_succeeded(&self) -> bool {
        self.failures.is_empty() && self.omissions.is_empty()
    }
}
