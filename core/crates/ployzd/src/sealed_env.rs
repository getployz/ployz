//! Opening a spec's Sealed env values on this Machine, and the record that
//! lets it hide them again in its own logs.

use std::{
    collections::{BTreeMap, VecDeque, btree_map::Entry},
    future::Future,
    io,
    path::Path,
};

use futures_util::StreamExt;
use ployz_core::scrub::{self, Channel, FrameScrub, SealedForms, SealedValues};
use ployz_core::{
    ContainerId, EnvPart, EnvValue, HistoryRow, OpaquePayload, SealedIdentity, SealedScope,
    SealedValue,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use crate::{
    logs::{LogLineStream, LogSource, RawLogEntry, RpcStream},
    sealing_key::SealingKey,
};

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
    #[error("env name {0:?} cannot hold a Sealed value")]
    BadName(String),
    #[error("env {0} holds a Sealed value bound to another variable")]
    Unbound(String),
    #[error("env {0} does not open with this Cluster's Sealing Key")]
    Unopenable(String),
    #[error("env {0} holds a multiline Sealed value with a line too short to scrub from logs")]
    ShortLine(String),
}

pub(crate) struct OpenedEnv {
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) record: Option<SealedRecord>,
}

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
    for opened in record.opened(&key)? {
        values.insert(opened.name.to_owned(), opened.whole);
    }
    Ok(OpenedEnv {
        values,
        record: Some(record),
    })
}

impl SealedRecord {
    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    pub(crate) fn from_label(label: Option<&str>) -> Result<Option<Self>, serde_json::Error> {
        label.map(serde_json::from_str).transpose()
    }

    pub(crate) fn secrets(&self, key_dir: Option<&Path>) -> Result<SealedValues, OpenError> {
        let key = load(key_dir)?;
        Ok(self
            .opened(&key)?
            .into_iter()
            .flat_map(|opened| {
                std::iter::once((opened.name.to_owned(), opened.whole)).chain(opened.parts)
            })
            .collect())
    }

    /// Open every value for a Build on this Machine: the values by name for
    /// its secret files, and the forms that hide them in what it reports.
    pub(crate) fn open_for_build(
        &self,
        key_dir: Option<&Path>,
    ) -> Result<(BTreeMap<String, String>, SealedForms), OpenError> {
        self.check_bindings()?;
        let key = load(key_dir)?;
        let opened = self.opened(&key)?;
        let secrets: SealedValues = opened
            .iter()
            .flat_map(|opened| {
                std::iter::once((opened.name.to_owned(), opened.whole.clone()))
                    .chain(opened.parts.iter().cloned())
            })
            .collect();
        let values = opened
            .into_iter()
            .map(|opened| (opened.name.to_owned(), opened.whole))
            .collect();
        Ok((values, SealedForms::compile(&secrets)))
    }

    fn opened(&self, key: &SealingKey) -> Result<Vec<Opened<'_>>, OpenError> {
        self.values
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
                            let plaintext = self.open(key, name, sealed)?;
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

    fn check_bindings(&self) -> Result<(), OpenError> {
        if self.values.contains_key("PORT") {
            return Err(OpenError::SealedPort);
        }
        for (name, value) in &self.values {
            if name.is_empty() || name.contains(['=', '\0']) {
                return Err(OpenError::BadName(name.clone()));
            }
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
    ) -> Result<String, OpenError> {
        key.open_value(&sealed.associated_data(&self.identity), &sealed.sealed)
            .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
            .ok_or_else(|| OpenError::Unopenable(name.to_owned()))
    }
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
        key_dir: Option<&Path>,
    ) -> Self {
        let forms = match record {
            Ok(None) => return Self::Shown,
            Ok(Some(record)) => record.secrets(key_dir).map_err(|error| error.to_string()),
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

    fn row(
        &self,
        payload: OpaquePayload,
        row: HistoryRow,
    ) -> Option<Result<OpaquePayload, Status>> {
        match (self, row) {
            (Self::Shown, _) => Some(Ok(payload)),
            (Self::Withheld, _) => None,
            (
                Self::Scrubbed(forms),
                HistoryRow::Line {
                    container_id,
                    ts,
                    stream,
                    text,
                },
            ) => Some(
                HistoryRow::Line {
                    container_id,
                    ts,
                    stream,
                    text: forms.scrub_bytes(&text, &mut 0),
                }
                .encode()
                .map_err(|error| ployz_core::rpc::caused_status(tonic::Code::Internal, &error)),
            ),
            (Self::Scrubbed(_), _) => Some(Ok(payload)),
        }
    }
}

pub(crate) fn scrub_history<F, Fut>(mut rows: RpcStream, lines_of: F) -> RpcStream
where
    F: Fn(ContainerId) -> Fut + Send + 'static,
    Fut: Future<Output = Lines> + Send,
{
    let (sender, receiver) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut containers = BTreeMap::new();
        while let Some(payload) = rows.next().await {
            let row = match payload {
                Err(status) => Some(Err(status)),
                Ok(payload) => match HistoryRow::decode(&payload) {
                    Ok(row @ HistoryRow::Line { container_id, .. }) => {
                        let lines = match containers.entry(container_id) {
                            Entry::Occupied(known) => known.into_mut(),
                            Entry::Vacant(first) => first.insert(lines_of(container_id).await),
                        };
                        lines.row(payload, row)
                    }
                    Ok(_) => Some(Ok(payload)),
                    Err(error) => Some(Err(ployz_core::rpc::caused_status(
                        tonic::Code::Internal,
                        &error,
                    ))),
                },
            };
            let failed = matches!(row, Some(Err(_)));
            if let Some(row) = row
                && sender.send(row).await.is_err()
            {
                return;
            }
            if failed {
                return;
            }
        }
    });
    ReceiverStream::new(receiver)
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

fn load(key_dir: Option<&Path>) -> Result<SealingKey, OpenError> {
    let dir = key_dir.ok_or(OpenError::KeyMissing)?;
    SealingKey::load(dir)
        .map_err(OpenError::KeyUnreadable)?
        .ok_or(OpenError::KeyMissing)
}

#[cfg(test)]
pub(crate) mod test_support {
    use uuid::Uuid;

    use super::*;
    use crate::test_dir::TestDir;

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

        pub(crate) fn open(&self, env: BTreeMap<String, EnvValue>) -> Result<OpenedEnv, OpenError> {
            open(env, Some(&self.identity), Some(&self.dir.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::Template;
    use uuid::Uuid;

    use super::test_support::Cluster;
    use super::*;
    use crate::{logs::JournalError, observe::layout::RetainedEnv, test_dir::TestDir};

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
        let secrets = record.secrets(Some(&cluster.dir.0)).unwrap();
        assert_eq!(format!("{secrets:?}"), r#"["PASSWORD", "TOKEN", "URL"]"#);
        let label = serde_json::to_string(&record).unwrap();
        assert_eq!(
            SealedRecord::from_label(Some(&label)).unwrap(),
            Some(record.clone())
        );
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
            open(url(), Some(&elsewhere), Some(&cluster.dir.0)),
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
        let secrets = opened
            .record
            .unwrap()
            .secrets(Some(&cluster.dir.0))
            .unwrap();
        assert_eq!(
            format!("{secrets:?}"),
            r#"["DB_PASSWORD", "DB_USER", "URL"]"#
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

    #[test]
    fn retained_env_keeps_an_opened_value_only_as_ciphertext() {
        let cluster = Cluster::new();
        let record = token_record(&cluster);
        let label = serde_json::to_string(&record).unwrap();
        let env = RetainedEnv::from_inspected(
            &["TOKEN=hunter2-secret".to_owned(), "MODE=prod".to_owned()],
            Some(&label),
        )
        .unwrap();
        let dir = TestDir::new("sealed-env-retained");
        std::fs::create_dir_all(&dir.0).unwrap();
        env.write(&dir.0).unwrap();
        let written = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|file| std::fs::read(file.unwrap().path()).unwrap())
            .collect::<Vec<_>>()
            .concat();
        assert!(!String::from_utf8_lossy(&written).contains("hunter2-secret"));
        let kept = RetainedEnv::read(&dir.0).unwrap().unwrap();
        assert_eq!(kept.sealed(), Some(&record));
        assert_eq!(
            kept.into_values(),
            BTreeMap::from([("MODE".to_owned(), "prod".to_owned())])
        );
        assert!(RetainedEnv::from_inspected(&[], Some("not json")).is_none());
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
        let secrets = token_record(&cluster)
            .secrets(Some(&cluster.dir.0))
            .unwrap();
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

    fn line(container: u8, text: &str) -> OpaquePayload {
        row(HistoryRow::Line {
            container_id: ContainerId::parse(format!("{container:x}").repeat(64)).unwrap(),
            ts: 1,
            stream: ployz_core::stream::HistoryStream::Stdout,
            text: text.as_bytes().to_vec(),
        })
    }

    fn row(row: HistoryRow) -> OpaquePayload {
        row.encode().unwrap()
    }

    async fn history(
        rows: Vec<Result<OpaquePayload, Status>>,
        lines_of: impl Fn(ContainerId) -> Lines + Send + Sync + 'static,
    ) -> Vec<Result<HistoryRow, tonic::Code>> {
        let (sender, receiver) = mpsc::channel(rows.len().max(1));
        for row in rows {
            sender.send(row).await.unwrap();
        }
        drop(sender);
        let lines_of = std::sync::Arc::new(lines_of);
        scrub_history(ReceiverStream::new(receiver), move |id| {
            let lines_of = lines_of.clone();
            async move { lines_of(id) }
        })
        .map(|row| {
            row.map(|payload| HistoryRow::decode(&payload).unwrap())
                .map_err(|status| status.code())
        })
        .collect()
        .await
    }

    fn text(row: &Result<HistoryRow, tonic::Code>) -> Option<String> {
        match row {
            Ok(HistoryRow::Line { text, .. }) => Some(String::from_utf8(text.clone()).unwrap()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn history_scrubs_or_withholds_each_containers_lines() {
        let cluster = Cluster::new();
        let record = token_record(&cluster);
        let key_dir = cluster.dir.0.clone();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        let rows = history(
            vec![
                Ok(line(1, "token=hunter2-secret")),
                Ok(line(2, "hunter2-secret leaked")),
                Ok(line(3, "plain hunter2-secret")),
                Ok(row(HistoryRow::Heartbeat)),
                Ok(line(1, "again hunter2-secret")),
            ],
            move |id| {
                seen.lock().unwrap().push(id);
                let record = match id.as_str().chars().next() {
                    Some('1') => Ok(Some(record.clone())),
                    Some('2') => Err(Status::unavailable("no record")),
                    _ => Ok(None),
                };
                Lines::new(id, record, Some(&key_dir))
            },
        )
        .await;
        assert!(matches!(rows.get(2), Some(Ok(HistoryRow::Heartbeat))));
        assert_eq!(
            rows.iter().map(text).collect::<Vec<_>>(),
            [
                Some("token=[secret TOKEN]".to_owned()),
                Some("plain hunter2-secret".to_owned()),
                None,
                Some("again [secret TOKEN]".to_owned()),
            ]
        );
        assert_eq!(asked.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn history_withholds_lines_when_the_key_is_gone() {
        let cluster = Cluster::new();
        let record = token_record(&cluster);
        let empty = TestDir::new("sealed-env-history-no-key");
        std::fs::create_dir_all(&empty.0).unwrap();
        let empty_dir = empty.0.clone();
        let rows = history(vec![Ok(line(1, "hunter2-secret"))], move |id| {
            Lines::new(id, Ok::<_, Status>(Some(record.clone())), Some(&empty_dir))
        })
        .await;
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn history_ends_at_the_first_error() {
        let rows = history(
            vec![
                Ok(line(1, "a")),
                Err(Status::internal("broken")),
                Ok(line(1, "b")),
            ],
            |_| Lines::Shown,
        )
        .await;
        assert!(matches!(
            rows.as_slice(),
            [Ok(_), Err(tonic::Code::Internal)]
        ));
    }
}
