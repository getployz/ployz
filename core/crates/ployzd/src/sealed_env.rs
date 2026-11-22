//! Opening a spec's Sealed env values on this Machine, and the record that
//! lets it hide them again in its own logs.

use std::{
    collections::{BTreeMap, VecDeque},
    io,
    path::PathBuf,
};

use futures_util::StreamExt;
use ployz_core::scrub::{self, Channel, FrameScrub, SealedForms, SealedValues};
use ployz_core::{
    Ciphertext, ContainerId, EnvPart, EnvValue, OpenedSealed, PlatformPurpose, SealedIdentity,
    SealedScope, SealedValue, authority::ValueReach,
};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    logs::{LogLineStream, LogSource, RawLogEntry},
    machine::AdmittedSpec,
    sealing_key::SealingKey,
};

pub use ployz_core::SealedRecord;

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
    #[error("env name {0:?} cannot hold a Sealed value")]
    BadName(String),
    #[error("env {0} holds a Sealed value bound to another variable")]
    Unbound(String),
    #[error("env {0} does not open with this Cluster's Sealing Key")]
    Unopenable(String),
    #[error("the tunnel token does not open with this Cluster's Sealing Key")]
    TunnelToken,
    #[error("the Tailscale auth key does not open with this Cluster's Sealing Key")]
    PrivateAccessKey,
    #[error("env {0} holds a multiline Sealed value with a line too short to scrub from logs")]
    ShortLine(String),
    #[error("env {0} is Sealed, and nothing signed the spec")]
    Unsigned(String),
    #[error("env {0} holds a Sealed part the key that approved the spec doesn't reach")]
    OutOfReach(String),
}

pub(crate) struct OpenedEnv {
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) record: Option<SealedRecord>,
}

/// Where this Machine keeps its Sealing Key, read afresh at each open. A Machine given
/// none holds no key, and every open answers [`OpenError::KeyMissing`].
#[derive(Clone, Debug, Default)]
pub struct SealingKeyStore {
    data_dir: Option<PathBuf>,
}

impl SealingKeyStore {
    /// The Sealing Key kept in `data_dir`.
    #[must_use]
    pub fn at(data_dir: PathBuf) -> Self {
        Self {
            data_dir: Some(data_dir),
        }
    }

    fn load(&self) -> Result<SealingKey, OpenError> {
        let dir = self.data_dir.as_deref().ok_or(OpenError::KeyMissing)?;
        SealingKey::load(dir)
            .map_err(OpenError::KeyUnreadable)?
            .ok_or(OpenError::KeyMissing)
    }

    /// Open `environment` for a Container of the spec `admitted` stands for. A Sealed
    /// value opens only for a spec a signature stands behind, and each of its Sealed
    /// parts only when that signature reaches it, as [`AdmittedSpec::opens`] says.
    ///
    /// # Errors
    /// Returns [`OpenError::Unsigned`] for a Sealed value under an unsigned spec,
    /// [`OpenError::OutOfReach`] for a part the admission doesn't reach, and the other
    /// [`OpenError`]s when a value does not open.
    pub(crate) fn open_env(
        &self,
        environment: BTreeMap<String, EnvValue>,
        identity: Option<&SealedIdentity>,
        admitted: &AdmittedSpec,
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
        if !admitted.signed() {
            return Err(OpenError::Unsigned(first.clone()));
        }
        let identity = identity.ok_or_else(|| OpenError::MissingIdentity(first.clone()))?;
        for (name, value) in &sealed {
            let reached = value.sealed_parts().into_iter().all(|part| {
                admitted.opens(&ValueReach {
                    environment: identity.environment,
                    scope: part.scope,
                })
            });
            if !reached {
                return Err(OpenError::OutOfReach(name.clone()));
            }
        }
        let record = SealedRecord::new(identity.clone(), sealed);
        check_bindings(&record)?;
        let key = self.load()?;
        for opened in opened(&record, &key)? {
            values.insert(opened.name.to_owned(), opened.whole);
        }
        Ok(OpenedEnv {
            values,
            record: Some(record),
        })
    }

    /// Every opened value of `record`, whole and in parts, to hide them in what the
    /// Container writes.
    ///
    /// # Errors
    /// Returns the [`OpenError`] when the key is missing or a value does not open.
    pub(crate) fn secrets(&self, record: &SealedRecord) -> Result<SealedValues, OpenError> {
        let key = self.load()?;
        Ok(opened(record, &key)?
            .into_iter()
            .flat_map(|opened| {
                std::iter::once((opened.name.to_owned(), opened.whole)).chain(opened.parts)
            })
            .collect())
    }

    /// Open every value of `record` for a Build: each whole by name, with the parts that
    /// hide them in what it reports.
    ///
    /// # Errors
    /// Returns the [`OpenError`] when a value is bound to another name, the key is
    /// missing, or a value does not open.
    pub(crate) fn open_for_build(&self, record: &SealedRecord) -> Result<OpenedSealed, OpenError> {
        check_bindings(record)?;
        let key = self.load()?;
        let mut values = BTreeMap::new();
        let mut parts = Vec::new();
        for opened in opened(record, &key)? {
            values.insert(opened.name.to_owned(), opened.whole);
            parts.extend(opened.parts);
        }
        Ok(OpenedSealed::new(values, parts))
    }

    /// Open a value the platform sealed to this Cluster for `purpose`.
    ///
    /// # Errors
    ///
    /// Returns [`OpenError::KeyMissing`] or [`OpenError::KeyUnreadable`] when this
    /// Machine holds no usable Sealing Key, and [`OpenError::TunnelToken`] or
    /// [`OpenError::PrivateAccessKey`] when the value was sealed for another Cluster or
    /// another purpose.
    pub(crate) fn open_platform(
        &self,
        purpose: PlatformPurpose,
        sealed: &Ciphertext,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        self.load()?
            .open_platform(purpose, sealed)
            .ok_or(match purpose {
                PlatformPurpose::IngressTunnel => OpenError::TunnelToken,
                PlatformPurpose::PrivateAccess => OpenError::PrivateAccessKey,
            })
    }

    /// The Cluster's build attest key pair, derived from the Sealing Key.
    ///
    /// # Errors
    /// Returns [`OpenError::KeyMissing`] or [`OpenError::KeyUnreadable`] as opening does.
    pub(crate) fn attest(&self) -> Result<ployz_core::X25519Secret, OpenError> {
        Ok(self.load()?.attest())
    }
}

pub(crate) fn record_from_label(
    label: Option<&str>,
) -> Result<Option<SealedRecord>, serde_json::Error> {
    label.map(serde_json::from_str).transpose()
}

fn opened<'a>(record: &'a SealedRecord, key: &SealingKey) -> Result<Vec<Opened<'a>>, OpenError> {
    record
        .values()
        .iter()
        .map(|(name, value)| {
            let mut opened = Opened {
                name,
                whole: String::new(),
                parts: Vec::new(),
            };
            for part in parts(value) {
                match part {
                    Part::Text(text) => opened.whole.push_str(text),
                    Part::Sealed(sealed) => {
                        let plaintext = open_value(record, key, name, sealed)?;
                        if scrub::has_short_row(&plaintext) {
                            return Err(OpenError::ShortLine(name.clone()));
                        }
                        opened.whole.push_str(&plaintext);
                        opened.parts.push((sealed.key.clone(), plaintext));
                    }
                }
            }
            Ok(opened)
        })
        .collect()
}

fn check_bindings(record: &SealedRecord) -> Result<(), OpenError> {
    if record.values().contains_key("PORT") {
        return Err(OpenError::SealedPort);
    }
    for (name, value) in record.values() {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(OpenError::BadName(name.clone()));
        }
        if let EnvValue::Sealed(sealed) = value {
            let scope_matches = match sealed.scope {
                SealedScope::Shared => true,
                SealedScope::Service(service) => service == record.identity().service,
            };
            if sealed.key != *name || !scope_matches {
                return Err(OpenError::Unbound(name.clone()));
            }
        }
    }
    Ok(())
}

fn open_value(
    record: &SealedRecord,
    key: &SealingKey,
    name: &str,
    sealed: &SealedValue,
) -> Result<String, OpenError> {
    key.open_value(&sealed.associated_data(record.identity()), &sealed.sealed)
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .ok_or_else(|| OpenError::Unopenable(name.to_owned()))
}

pub(crate) enum Lines {
    Shown,
    Scrubbed(SealedForms),
    Withheld,
}

impl Lines {
    pub(crate) fn new(
        container: ContainerId,
        record: Result<Option<SealedRecord>, impl std::fmt::Display>,
        sealing: &SealingKeyStore,
    ) -> Self {
        let forms = match record {
            Ok(None) => return Self::Shown,
            Ok(Some(record)) => sealing.secrets(&record).map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        match forms {
            Ok(secrets) => Self::Scrubbed(SealedForms::compile(&secrets)),
            Err(error) => {
                tracing::warn!(%container, %error, "withholding text whose Sealed values cannot be learned");
                Self::Withheld
            }
        }
    }

    /// `text` with each opened value hidden, or nothing when it is withheld.
    pub(crate) fn text(&self, text: &str) -> Option<String> {
        match self {
            Self::Shown => Some(text.to_owned()),
            Self::Scrubbed(forms) => Some(forms.scrub(text, &mut 0)),
            Self::Withheld => None,
        }
    }
}

pub(crate) fn scrub_source(source: LogSource, forms: SealedForms) -> LogSource {
    struct Scrubbing {
        source: LogSource,
        forms: SealedForms,
        frames: Option<FrameScrub>,
        last: i64,
        held: VecDeque<RawLogEntry>,
    }
    let scrubbing = Scrubbing {
        source,
        forms,
        frames: Some(FrameScrub::new()),
        last: 0,
        held: VecDeque::new(),
    };
    Box::pin(futures_util::stream::unfold(
        scrubbing,
        |mut state| async move {
            loop {
                if let Some(entry) = state.held.pop_front() {
                    return Some((Ok(entry), state));
                }
                let frames = state.frames.as_mut()?;
                match state.source.next().await {
                    Some(Ok(entry)) => {
                        state.last = entry.timestamp_unix_nanos;
                        let message =
                            frames.push(channel(entry.stream), &entry.message, &state.forms);
                        if !message.is_empty() {
                            return Some((Ok(RawLogEntry { message, ..entry }), state));
                        }
                    }
                    Some(Err(error)) => return Some((Err(error), state)),
                    None => {
                        let last = state.last;
                        let rest = state.frames.take()?.finish(&state.forms);
                        state.held.extend(
                            rest.into_iter()
                                .filter(|(_, message)| !message.is_empty())
                                .map(|(channel, message)| RawLogEntry {
                                    stream: match channel {
                                        Channel::Stdout => LogLineStream::Stdout,
                                        Channel::Stderr => LogLineStream::Stderr,
                                    },
                                    timestamp_unix_nanos: last,
                                    message,
                                }),
                        );
                    }
                }
            }
        },
    ))
}

fn channel(stream: LogLineStream) -> Channel {
    match stream {
        LogLineStream::Stdout => Channel::Stdout,
        LogLineStream::Stderr => Channel::Stderr,
    }
}

struct Opened<'a> {
    name: &'a str,
    whole: String,
    parts: Vec<(String, String)>,
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

#[cfg(test)]
pub(crate) mod test_support {
    use uuid::Uuid;

    use super::*;
    use crate::{sealing_key::SealingKey, test_dir::TestDir};

    pub(crate) struct Cluster {
        pub(crate) dir: TestDir,
        key: SealingKey,
        pub(crate) identity: SealedIdentity,
    }

    impl Cluster {
        pub(crate) fn new() -> Self {
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

        pub(crate) fn seal(&self, key: &str, scope: SealedScope, plaintext: &str) -> SealedValue {
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

        /// This Cluster's Sealing Key, as a Machine holding it keeps it.
        pub(crate) fn store(&self) -> SealingKeyStore {
            SealingKeyStore::at(self.dir.0.clone())
        }

        pub(crate) fn open(&self, env: BTreeMap<String, EnvValue>) -> Result<OpenedEnv, OpenError> {
            self.store()
                .open_env(env, Some(&self.identity), &AdmittedSpec::test_unbound(true))
        }

        /// `plaintext` sealed to this Cluster for `purpose`, as a client seals it.
        pub(crate) fn seal_platform(
            &self,
            purpose: PlatformPurpose,
            plaintext: &[u8],
        ) -> Ciphertext {
            ployz_core::seal_platform(&self.key.public_key(), purpose, plaintext).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::Template;
    use uuid::Uuid;

    use super::test_support::Cluster;
    use super::*;
    use crate::{logs::JournalError, test_dir::TestDir};

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
        assert_eq!(
            opened.values,
            BTreeMap::from([
                ("GREETING".to_owned(), "hi".to_owned()),
                ("TOKEN".to_owned(), "t0ken".to_owned()),
                ("URL".to_owned(), "postgres://app:pa55@db".to_owned()),
            ])
        );
        let record = opened.record.unwrap();
        assert_eq!(record.keys().collect::<Vec<_>>(), ["TOKEN", "URL"]);
        let secrets = cluster.store().secrets(&record).unwrap();
        assert_eq!(format!("{secrets:?}"), r#"["PASSWORD", "TOKEN", "URL"]"#);
        let label = serde_json::to_string(&record).unwrap();
        assert_eq!(
            record_from_label(Some(&label)).unwrap(),
            Some(record.clone())
        );
        assert!(!serde_json::to_string(&record).unwrap().contains("t0ken"));
    }

    #[test]
    fn text_only_env_needs_no_key_or_identity() {
        let opened = SealingKeyStore::default()
            .open_env(
                BTreeMap::from([("A".to_owned(), EnvValue::from("b"))]),
                None,
                &AdmittedSpec::test_unbound(true),
            )
            .unwrap();
        assert_eq!(opened.values.get("A").map(String::as_str), Some("b"));
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
    fn a_name_docker_would_split_or_drop_holds_no_sealed_value() {
        let cluster = Cluster::new();
        for name in ["TOKEN=alias", "", "TO\0KEN"] {
            let env = BTreeMap::from([(
                name.to_owned(),
                EnvValue::Template(Template {
                    template: vec![EnvPart::Sealed(cluster.seal("TOKEN", own(), "x"))],
                }),
            )]);
            assert!(
                matches!(cluster.open(env), Err(OpenError::BadName(bad)) if bad == name),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_template_part_opens_for_any_service_in_its_environment_and_no_other() {
        let cluster = Cluster::new();
        let url = || {
            BTreeMap::from([(
                "URL".to_owned(),
                EnvValue::Template(Template {
                    template: vec![
                        EnvPart::Text("postgres://app:".into()),
                        EnvPart::Sealed(cluster.seal(
                            "DB_PASSWORD",
                            SealedScope::Service(Uuid::from_u128(9)),
                            "pa55",
                        )),
                        EnvPart::Text("@db".into()),
                    ],
                }),
            )])
        };
        let elsewhere = SealedIdentity {
            environment: Uuid::from_u128(5),
            ..cluster.identity.clone()
        };

        assert_eq!(
            cluster
                .open(url())
                .unwrap()
                .values
                .get("URL")
                .map(String::as_str),
            Some("postgres://app:pa55@db")
        );
        assert!(matches!(
            cluster.store().open_env(url(), Some(&elsewhere), &AdmittedSpec::test_unbound(true)),
            Err(OpenError::Unopenable(key)) if key == "URL"
        ));
    }

    #[test]
    fn a_template_opens_every_sealed_part_in_place() {
        let cluster = Cluster::new();
        let env = BTreeMap::from([(
            "URL".to_owned(),
            EnvValue::Template(Template {
                template: vec![
                    EnvPart::Text("postgres://".into()),
                    EnvPart::Sealed(cluster.seal("DB_USER", own(), "app")),
                    EnvPart::Text(":".into()),
                    EnvPart::Sealed(cluster.seal("DB_PASSWORD", SealedScope::Shared, "pa55")),
                    EnvPart::Text("@db".into()),
                ],
            }),
        )]);

        let opened = cluster.open(env).unwrap();

        assert_eq!(
            opened.values.get("URL").map(String::as_str),
            Some("postgres://app:pa55@db")
        );
        let secrets = cluster.store().secrets(&opened.record.unwrap()).unwrap();
        assert_eq!(
            format!("{secrets:?}"),
            r#"["DB_PASSWORD", "DB_USER", "URL"]"#
        );
    }

    #[test]
    fn a_template_part_opens_only_when_the_admission_reached_it() {
        let cluster = Cluster::new();
        let env = BTreeMap::from([(
            "URL".to_owned(),
            EnvValue::Template(Template {
                template: vec![
                    EnvPart::Sealed(cluster.seal("DB_USER", own(), "app")),
                    EnvPart::Sealed(cluster.seal(
                        "DB_PASSWORD",
                        SealedScope::Service(Uuid::from_u128(9)),
                        "pa55",
                    )),
                ],
            }),
        )]);
        let part = |scope| ValueReach {
            environment: cluster.identity.environment,
            scope,
        };
        let open_reaching = |values| {
            cluster.store().open_env(
                env.clone(),
                Some(&cluster.identity),
                &AdmittedSpec::test_reaching(ployz_core::authority::Reach {
                    values,
                    volumes: Vec::new(),
                }),
            )
        };

        assert!(matches!(
            open_reaching(vec![part(own())]),
            Err(OpenError::OutOfReach(key)) if key == "URL"
        ));
        assert_eq!(
            open_reaching(vec![
                part(own()),
                part(SealedScope::Service(Uuid::from_u128(9)))
            ])
            .unwrap()
            .values
            .get("URL")
            .map(String::as_str),
            Some("apppa55")
        );
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
            cluster.store().open_env(env, Some(&elsewhere), &AdmittedSpec::test_unbound(true)),
            Err(OpenError::Unopenable(key)) if key == "TOKEN"
        ));
    }

    #[test]
    fn a_sealed_value_opens_only_for_a_spec_something_signed() {
        let cluster = Cluster::new();
        let token = BTreeMap::from([(
            "TOKEN".to_owned(),
            EnvValue::Sealed(cluster.seal("TOKEN", own(), "x")),
        )]);
        assert!(matches!(
            cluster.store().open_env(token, Some(&cluster.identity), &AdmittedSpec::test_unbound(false)),
            Err(OpenError::Unsigned(key)) if key == "TOKEN"
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
            SealingKeyStore::default().open_env(
                token(),
                Some(&cluster.identity),
                &AdmittedSpec::test_unbound(true)
            ),
            Err(OpenError::KeyMissing)
        ));
        let empty = TestDir::new("sealed-env-no-key");
        std::fs::create_dir_all(&empty.0).unwrap();
        assert!(matches!(
            SealingKeyStore::at(empty.0.clone()).open_env(
                token(),
                Some(&cluster.identity),
                &AdmittedSpec::test_unbound(true)
            ),
            Err(OpenError::KeyMissing)
        ));
        assert!(matches!(
            cluster
                .store()
                .open_env(token(), None, &AdmittedSpec::test_unbound(true)),
            Err(OpenError::MissingIdentity(_))
        ));
        let port = BTreeMap::from([(
            "PORT".to_owned(),
            EnvValue::Sealed(cluster.seal("PORT", own(), "80")),
        )]);
        assert!(matches!(cluster.open(port), Err(OpenError::SealedPort)));
    }

    #[test]
    fn a_multiline_sealed_value_with_a_short_line_is_refused_without_echoing_it() {
        let cluster = Cluster::new();
        let leaky = "line-one-QX7MARKER\nZq9\n";
        let direct = EnvValue::Sealed(cluster.seal("CERT", own(), leaky));
        let in_template = EnvValue::Template(Template {
            template: vec![
                EnvPart::Text("pem:".into()),
                EnvPart::Sealed(cluster.seal("KEY", own(), leaky)),
            ],
        });
        for value in [direct, in_template] {
            let refused = cluster
                .open(BTreeMap::from([("CERT".to_owned(), value)]))
                .err()
                .unwrap();
            assert!(matches!(&refused, OpenError::ShortLine(key) if key == "CERT"));
            let mut shown = vec![refused.to_string(), format!("{refused:?}")];
            let mut source = std::error::Error::source(&refused);
            while let Some(cause) = source {
                shown.extend([cause.to_string(), format!("{cause:?}")]);
                source = cause.source();
            }
            for part in ["QX7MARKER", "Zq9"] {
                assert!(shown.iter().all(|text| !text.contains(part)), "{shown:?}");
            }
        }
    }

    #[test]
    fn template_text_and_a_short_value_on_one_line_are_not_refused() {
        let cluster = Cluster::new();
        let env = BTreeMap::from([
            (
                "CERT".to_owned(),
                EnvValue::Template(Template {
                    template: vec![
                        EnvPart::Text("a\nb\n".into()),
                        EnvPart::Sealed(cluster.seal("KEY", own(), "line-one\n\nline-three\n")),
                    ],
                }),
            ),
            (
                "PIN".to_owned(),
                EnvValue::Sealed(cluster.seal("PIN", own(), "4821")),
            ),
        ]);
        let opened = cluster.open(env).unwrap();
        assert_eq!(
            opened.values,
            BTreeMap::from([
                (
                    "CERT".to_owned(),
                    "a\nb\nline-one\n\nline-three\n".to_owned()
                ),
                ("PIN".to_owned(), "4821".to_owned()),
            ])
        );
    }

    fn token_record(cluster: &Cluster) -> SealedRecord {
        let env = BTreeMap::from([(
            "TOKEN".to_owned(),
            EnvValue::Sealed(cluster.seal("TOKEN", own(), "hunter2-secret")),
        )]);
        cluster.open(env).unwrap().record.unwrap()
    }

    fn entry(ts: i64, message: &str) -> Result<RawLogEntry, JournalError> {
        Ok(RawLogEntry {
            stream: LogLineStream::Stdout,
            timestamp_unix_nanos: ts,
            message: message.as_bytes().to_vec(),
        })
    }

    #[tokio::test]
    async fn a_live_log_never_shows_a_value_split_across_frames() {
        let cluster = Cluster::new();
        let secrets = cluster.store().secrets(&token_record(&cluster)).unwrap();
        let source: LogSource = Box::pin(futures_util::stream::iter([
            entry(1, "login hunter2-"),
            entry(2, "secret ok\nbye hunter2-sec"),
            entry(3, "ret"),
        ]));
        let entries: Vec<_> = scrub_source(source, SealedForms::compile(&secrets))
            .map(Result::unwrap)
            .collect()
            .await;
        let text: String = entries
            .iter()
            .map(|entry| String::from_utf8_lossy(&entry.message))
            .collect();
        assert_eq!(text, "login [secret TOKEN] ok\nbye [secret TOKEN]");
        assert_eq!(entries.last().unwrap().timestamp_unix_nanos, 3);
    }

    #[tokio::test]
    async fn a_live_log_passes_a_read_error_through() {
        let source: LogSource = Box::pin(futures_util::stream::iter([
            entry(1, "a\n"),
            Err(JournalError::Docker("gone".into())),
        ]));
        let items: Vec<_> = scrub_source(source, SealedForms::none()).collect().await;
        assert!(matches!(
            items.as_slice(),
            [Ok(_), Err(JournalError::Docker(_)), ..]
        ));
    }
}
