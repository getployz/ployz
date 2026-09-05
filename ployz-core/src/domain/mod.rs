mod certificate_policy;
mod cluster_dns;
mod data_loss;
mod deploy;
mod hostname;
mod ingress;
mod issuance;
mod machine;
mod observation;
mod relay_endpoint;
mod runtime_watch;
mod selector;
mod service_graph;
mod spec;
mod volume;

pub use certificate_policy::*;
pub use cluster_dns::*;
pub use data_loss::*;
pub use deploy::*;
pub use hostname::*;
pub use ingress::*;
pub use issuance::*;
pub use machine::*;
pub use observation::*;
pub use relay_endpoint::*;
pub use runtime_watch::*;
pub use selector::*;
pub use service_graph::*;
pub use spec::*;
pub use volume::*;

use serde::{Deserialize, Serialize};

use crate::MachineId;

/// HTTP path served by the Ingress Proxy to prove Machine reachability.
pub const INGRESS_VERIFY_PATH: &str = "/.ployz-verify";

/// A name lookup result. Duplicate names are a normal observable state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "match", content = "values")]
pub enum NameMatches<T> {
    None,
    One(T),
    Ambiguous {
        // Require both fields even when T can deserialize a missing value (e.g. Option).
        #[serde(deserialize_with = "Deserialize::deserialize")]
        first: T,
        #[serde(deserialize_with = "Deserialize::deserialize")]
        second: T,
        rest: Vec<T>,
    },
}

impl<T> NameMatches<T> {
    #[must_use]
    pub fn from_matches(matches: Vec<T>) -> Self {
        let mut matches = matches.into_iter();
        let Some(first) = matches.next() else {
            return Self::None;
        };
        match matches.next() {
            None => Self::One(first),
            Some(second) => Self::Ambiguous {
                first,
                second,
                rest: matches.collect(),
            },
        }
    }

    /// Candidates in their original observation order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let (first, second, rest) = match self {
            Self::None => (None, None, &[][..]),
            Self::One(first) => (Some(first), None, &[][..]),
            Self::Ambiguous {
                first,
                second,
                rest,
            } => (Some(first), Some(second), rest.as_slice()),
        };
        first.into_iter().chain(second).chain(rest)
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
#[serde(bound(
    serialize = "T: Serialize, E: Serialize",
    deserialize = "T: Deserialize<'de>, E: Deserialize<'de>"
))]
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
