//! What a `confirmation_required` refusal of a removal or a deploy asks to confirm.
//!
//! The Store writes it into the refusal's `details`; the tools turn it into a retry
//! and the CLI into a prompt and a command to run again. Every one of them reads the
//! same [`Confirmation`], so the keys (`volumes`, `accept`, `version`, `effects`,
//! `effects_by_environment`) have one owner. They are part of the frozen wire shape:
//! additive only.

use std::collections::BTreeMap;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::review::DestructiveEffect;
use crate::{EnvironmentName, VolumeName};

/// The Volumes whose data a confirmation accepts losing, by name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Accept {
    /// One Environment's Volumes, as a deploy or a publish asks.
    Names(Vec<VolumeName>),
    /// Each Environment's Volumes, as a removal asks.
    ByEnvironment(BTreeMap<EnvironmentName, Vec<VolumeName>>),
}

impl Default for Accept {
    fn default() -> Self {
        Self::Names(Vec::new())
    }
}

impl Accept {
    /// Every Volume name listed, once each however the list is grouped.
    #[must_use]
    pub fn names(&self) -> Vec<&VolumeName> {
        match self {
            Self::Names(names) => names.iter().collect(),
            Self::ByEnvironment(by_environment) => by_environment.values().flatten().collect(),
        }
    }
}

/// The evidence of a `confirmation_required` refusal that deletes data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Confirmation {
    /// The Volumes found holding data, as the Store reviewed them.
    pub volumes: Value,
    /// The Volumes the caller must name to go ahead.
    pub accept: Option<Accept>,
    /// What the confirmation binds: pass it back with the names.
    pub version: Option<String>,
    /// What a removal deletes, in all.
    pub effects: Option<Vec<DestructiveEffect>>,
    /// The same effects by the Environment they remove from.
    pub effects_by_environment: Option<BTreeMap<EnvironmentName, Vec<DestructiveEffect>>>,
}

impl Confirmation {
    /// A refusal saying `message` with this as its `details`.
    pub(crate) fn refuse(self, message: impl Into<String>) -> RpcError {
        let mut details = serde_json::Map::new();
        details.insert("volumes".to_owned(), self.volumes);
        details.insert(
            "accept".to_owned(),
            serde_json::to_value(self.accept.unwrap_or_default()).expect("an accept list is JSON"),
        );
        if let Some(effects) = self.effects {
            details.insert(
                "effects".to_owned(),
                serde_json::to_value(effects).expect("effects are JSON"),
            );
        }
        if let Some(by_environment) = self.effects_by_environment {
            details.insert(
                "effects_by_environment".to_owned(),
                serde_json::to_value(by_environment).expect("effects are JSON"),
            );
        }
        details.insert("version".to_owned(), self.version.into());
        super::confirmation_required(message, Value::Object(details))
    }

    /// What `details` carries of a confirmation. A key that is absent or isn't in the
    /// shape the Store writes reads as absent.
    #[must_use]
    pub fn of(details: &Value) -> Self {
        let read = |key: &str| details.get(key).cloned();
        Self {
            volumes: read("volumes").unwrap_or(Value::Null),
            accept: read("accept").and_then(|accept| serde_json::from_value(accept).ok()),
            version: read("version").and_then(|version| version.as_str().map(str::to_owned)),
            effects: read("effects").and_then(|effects| serde_json::from_value(effects).ok()),
            effects_by_environment: read("effects_by_environment")
                .and_then(|by| serde_json::from_value(by).ok()),
        }
    }
}

/// The message of a refusal to delete the data of `names`, which `subject` does:
/// "This permanently deletes" for a deploy, "Next deploy deletes" for a publish.
pub(crate) fn data_loss_message<'a>(
    subject: &str,
    names: impl IntoIterator<Item = &'a VolumeName>,
) -> String {
    let names: Vec<&str> = names.into_iter().map(VolumeName::as_str).collect();
    format!(
        "{subject} the data of {}. Accept each by name",
        names.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::review::DestructiveKind;

    fn volume(name: &str) -> VolumeName {
        VolumeName::parse(name).unwrap()
    }

    fn effect() -> DestructiveEffect {
        DestructiveEffect {
            kind: DestructiveKind::DeletesVolume,
            name: "data".to_owned(),
            node: "vol-1".to_owned(),
            path: "/volumes/data".to_owned(),
            lost_after: None,
        }
    }

    // The details a deploy's refusal carried when it wrote them as a `json!` literal.
    #[test]
    fn a_volume_loss_confirmation_writes_the_details_it_always_did() {
        let refusal = Confirmation {
            volumes: json!([{ "name": "data" }]),
            accept: Some(Accept::Names(vec![volume("data"), volume("logs")])),
            version: Some("3:1:0.1:9f2c".to_owned()),
            ..Confirmation::default()
        }
        .refuse("This deletes data");
        assert_eq!(
            refusal.details,
            json!({
                "volumes": [{ "name": "data" }],
                "accept": ["data", "logs"],
                "version": "3:1:0.1:9f2c",
            })
        );
    }

    #[test]
    fn a_removal_confirmation_writes_the_details_it_always_did() {
        let refusal = Confirmation {
            volumes: json!({ "production": [] }),
            accept: Some(Accept::ByEnvironment(
                [(
                    EnvironmentName::parse("production").unwrap(),
                    vec![volume("data")],
                )]
                .into(),
            )),
            version: Some("v1".to_owned()),
            effects: Some(vec![effect()]),
            effects_by_environment: Some(
                [(
                    EnvironmentName::parse("production").unwrap(),
                    vec![effect()],
                )]
                .into(),
            ),
        }
        .refuse("Removing this will delete Volume data");
        assert_eq!(
            refusal.details,
            json!({
                "volumes": { "production": [] },
                "accept": { "production": ["data"] },
                "effects": [effect()],
                "effects_by_environment": { "production": [effect()] },
                "version": "v1",
            })
        );
        assert_eq!(
            Confirmation::of(&refusal.details).effects,
            Some(vec![effect()])
        );
    }

    #[test]
    fn what_is_absent_or_in_another_shape_reads_as_absent() {
        let read = Confirmation::of(&json!({ "accept": 3, "version": 4, "effects": "none" }));
        assert_eq!(read.accept, None);
        assert_eq!(read.version, None);
        assert_eq!(read.effects, None);
        assert_eq!(Confirmation::of(&Value::Null), Confirmation::default());
    }

    #[test]
    fn the_data_loss_message_reads_as_it_did_for_a_deploy_and_a_publish() {
        let names = [volume("data"), volume("logs")];
        assert_eq!(
            data_loss_message("This permanently deletes", &names),
            "This permanently deletes the data of data, logs. Accept each by name"
        );
        assert_eq!(
            data_loss_message("Next deploy deletes", &names),
            "Next deploy deletes the data of data, logs. Accept each by name"
        );
    }
}
