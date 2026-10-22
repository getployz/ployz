//! Opening a spec's Sealed env values on this Machine, and the record that
//! lets it hide them again in its own logs.

use std::{collections::BTreeMap, io, path::Path};

use ployz_core::{EnvPart, EnvValue, SealedIdentity, SealedScope, SealedValue};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::sealing_key::SealingKey;

/// Docker label holding a Container's [`SealedRecord`].
pub(crate) const LABEL_SEALED: &str = "ployz.env.sealed";

/// The Sealed values a Container was given, still sealed, keyed by the env
/// key they opened into.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SealedRecord {
    identity: SealedIdentity,
    values: BTreeMap<String, EnvValue>,
}

#[derive(Debug, Error)]
pub enum OpenError {
    #[error("this Machine holds no Sealing Key")]
    KeyMissing,
    #[error("cannot read this Machine's Sealing Key")]
    KeyUnreadable(#[source] io::Error),
    #[error("env {0} is Sealed but the spec names no Sealed identity")]
    MissingIdentity(String),
    #[error("env PORT cannot hold a Sealed value")]
    SealedPort,
    #[error("env {0} holds a Sealed value bound to another variable")]
    Unbound(String),
    #[error("env {0} does not open with this Cluster's Sealing Key")]
    Unopenable(String),
}

/// A Container's env with every Sealed value opened, and the record of them.
pub(crate) struct OpenedEnv {
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) record: Option<SealedRecord>,
}

/// Opens `environment` with the Sealing Key kept in `key_dir`, read only when
/// a value is Sealed.
pub(crate) fn open(
    environment: BTreeMap<String, EnvValue>,
    identity: Option<&SealedIdentity>,
    key_dir: Option<&Path>,
) -> Result<OpenedEnv, OpenError> {
    let (sealed, text): (BTreeMap<_, _>, BTreeMap<_, _>) = environment
        .into_iter()
        .partition(|(_, value)| value.is_sealed());
    let mut values: BTreeMap<String, String> = text
        .into_iter()
        .filter_map(|(key, value)| Some((key, value.text()?.to_owned())))
        .collect();
    let Some((first, _)) = sealed.first_key_value() else {
        return Ok(OpenedEnv {
            values,
            record: None,
        });
    };
    let identity = identity.ok_or_else(|| OpenError::MissingIdentity(first.clone()))?;
    let record = SealedRecord {
        identity: identity.clone(),
        values: sealed,
    };
    record.check_bindings()?;
    let key = load(key_dir)?;
    for (name, value) in &record.values {
        let mut opened = String::new();
        for part in parts(value) {
            match part {
                Part::Text(text) => opened.push_str(text),
                Part::Sealed(sealed) => opened.push_str(&record.open(&key, name, sealed)?),
            }
        }
        values.insert(name.clone(), opened);
    }
    Ok(OpenedEnv {
        values,
        record: Some(record),
    })
}

impl SealedRecord {
    /// The env keys whose values this Machine opened.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    /// Every opened Sealed part, to hide wherever it shows.
    pub(crate) fn secrets(
        &self,
        key_dir: Option<&Path>,
    ) -> Result<Vec<Zeroizing<String>>, OpenError> {
        let key = load(key_dir)?;
        self.values
            .iter()
            .flat_map(|(name, value)| {
                parts(value).filter_map(move |part| match part {
                    Part::Text(_) => None,
                    Part::Sealed(sealed) => Some((name, sealed)),
                })
            })
            .map(|(name, sealed)| self.open(&key, name, sealed))
            .collect()
    }

    fn check_bindings(&self) -> Result<(), OpenError> {
        if self.values.contains_key("PORT") {
            return Err(OpenError::SealedPort);
        }
        for (name, value) in &self.values {
            if let EnvValue::Sealed(sealed) = value {
                let scope_matches = match sealed.scope {
                    SealedScope::Shared => true,
                    SealedScope::Service(service) => service == self.identity.service,
                };
                if sealed.key != *name || !scope_matches {
                    return Err(OpenError::Unbound(name.clone()));
                }
            }
        }
        Ok(())
    }

    fn open(
        &self,
        key: &SealingKey,
        name: &str,
        sealed: &SealedValue,
    ) -> Result<Zeroizing<String>, OpenError> {
        key.open_value(&sealed.associated_data(&self.identity), &sealed.sealed)
            .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .map(Zeroizing::new)
            .ok_or_else(|| OpenError::Unopenable(name.to_owned()))
    }
}

enum Part<'a> {
    Text(&'a str),
    Sealed(&'a SealedValue),
}

fn parts(value: &EnvValue) -> Box<dyn Iterator<Item = Part<'_>> + '_> {
    match value {
        EnvValue::Text(text) => Box::new(std::iter::once(Part::Text(text))),
        EnvValue::Sealed(sealed) => Box::new(std::iter::once(Part::Sealed(sealed))),
        EnvValue::Template(template) => Box::new(template.template.iter().map(|part| match part {
            EnvPart::Text(text) => Part::Text(text),
            EnvPart::Sealed(sealed) => Part::Sealed(sealed),
        })),
    }
}

fn load(key_dir: Option<&Path>) -> Result<SealingKey, OpenError> {
    let dir = key_dir.ok_or(OpenError::KeyMissing)?;
    SealingKey::load(dir)
        .map_err(OpenError::KeyUnreadable)?
        .ok_or(OpenError::KeyMissing)
}

#[cfg(test)]
mod tests {
    use ployz_core::Template;
    use uuid::Uuid;

    use super::*;
    use crate::test_dir::TestDir;

    struct Cluster {
        dir: TestDir,
        key: SealingKey,
        identity: SealedIdentity,
    }

    impl Cluster {
        fn new() -> Self {
            let dir = TestDir::new("sealed-env");
            std::fs::create_dir_all(&dir.0).unwrap();
            let key = SealingKey::generate();
            SealingKey::set(&dir.0, Some(&key)).unwrap();
            Self {
                dir,
                key,
                identity: SealedIdentity {
                    organization: "org_1".into(),
                    environment: Uuid::from_u128(1),
                    service: Uuid::from_u128(2),
                },
            }
        }

        fn seal(&self, key: &str, scope: SealedScope, plaintext: &str) -> SealedValue {
            let mut value = SealedValue {
                sealed: ployz_core::Ciphertext::from_bytes(vec![0; 48]),
                scope,
                key: key.into(),
            };
            value.sealed = self
                .key
                .seal_value(&value.associated_data(&self.identity), plaintext.as_bytes());
            value
        }

        fn open(&self, env: BTreeMap<String, EnvValue>) -> Result<OpenedEnv, OpenError> {
            open(env, Some(&self.identity), Some(&self.dir.0))
        }
    }

    fn own() -> SealedScope {
        SealedScope::Service(Uuid::from_u128(2))
    }

    #[test]
    fn sealed_and_template_values_open_and_text_passes_through() {
        let cluster = Cluster::new();
        let env = BTreeMap::from([
            ("GREETING".to_owned(), EnvValue::from("hi")),
            (
                "TOKEN".to_owned(),
                EnvValue::Sealed(cluster.seal("TOKEN", own(), "t0ken")),
            ),
            (
                "URL".to_owned(),
                EnvValue::Template(Template {
                    template: vec![
                        EnvPart::Text("postgres://app:".into()),
                        EnvPart::Sealed(cluster.seal(
                            "PASSWORD",
                            SealedScope::Service(Uuid::from_u128(9)),
                            "pa55",
                        )),
                        EnvPart::Text("@db".into()),
                    ],
                }),
            ),
        ]);
        let opened = cluster.open(env).unwrap();
        assert_eq!(opened.values["GREETING"], "hi");
        assert_eq!(opened.values["TOKEN"], "t0ken");
        assert_eq!(opened.values["URL"], "postgres://app:pa55@db");
        let record = opened.record.unwrap();
        assert_eq!(record.keys().collect::<Vec<_>>(), ["TOKEN", "URL"]);
        let secrets = record.secrets(Some(&cluster.dir.0)).unwrap();
        let secrets: Vec<&str> = secrets.iter().map(|secret| secret.as_str()).collect();
        assert_eq!(secrets, ["t0ken", "pa55"]);
        assert!(!serde_json::to_string(&record).unwrap().contains("t0ken"));
    }

    #[test]
    fn text_only_env_needs_no_key_or_identity() {
        let opened = open(
            BTreeMap::from([("A".to_owned(), EnvValue::from("b"))]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(opened.values["A"], "b");
        assert!(opened.record.is_none());
    }

    #[test]
    fn a_direct_value_must_bind_to_its_destination_and_scope() {
        let cluster = Cluster::new();
        let renamed = BTreeMap::from([(
            "OTHER".to_owned(),
            EnvValue::Sealed(cluster.seal("TOKEN", own(), "x")),
        )]);
        assert!(matches!(cluster.open(renamed), Err(OpenError::Unbound(key)) if key == "OTHER"));
        let foreign = BTreeMap::from([(
            "TOKEN".to_owned(),
            EnvValue::Sealed(cluster.seal("TOKEN", SealedScope::Service(Uuid::from_u128(9)), "x")),
        )]);
        assert!(matches!(cluster.open(foreign), Err(OpenError::Unbound(_))));
    }

    #[test]
    fn a_value_sealed_for_another_environment_does_not_open() {
        let cluster = Cluster::new();
        let value = cluster.seal("TOKEN", SealedScope::Shared, "x");
        let elsewhere = SealedIdentity {
            environment: Uuid::from_u128(5),
            ..cluster.identity.clone()
        };
        let env = BTreeMap::from([("TOKEN".to_owned(), EnvValue::Sealed(value))]);
        assert!(matches!(
            open(env, Some(&elsewhere), Some(&cluster.dir.0)),
            Err(OpenError::Unopenable(key)) if key == "TOKEN"
        ));
    }

    #[test]
    fn refuses_without_key_identity_or_with_a_sealed_port() {
        let cluster = Cluster::new();
        let token = || {
            BTreeMap::from([(
                "TOKEN".to_owned(),
                EnvValue::Sealed(cluster.seal("TOKEN", own(), "x")),
            )])
        };
        assert!(matches!(
            open(token(), Some(&cluster.identity), None),
            Err(OpenError::KeyMissing)
        ));
        let empty = TestDir::new("sealed-env-no-key");
        std::fs::create_dir_all(&empty.0).unwrap();
        assert!(matches!(
            open(token(), Some(&cluster.identity), Some(&empty.0)),
            Err(OpenError::KeyMissing)
        ));
        assert!(matches!(
            open(token(), None, Some(&cluster.dir.0)),
            Err(OpenError::MissingIdentity(_))
        ));
        let port = BTreeMap::from([(
            "PORT".to_owned(),
            EnvValue::Sealed(cluster.seal("PORT", own(), "80")),
        )]);
        assert!(matches!(cluster.open(port), Err(OpenError::SealedPort)));
    }
}
