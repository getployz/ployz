//! Store warnings as one-line JSON on stderr, for a host that installs no `tracing`
//! subscriber of its own: Ployz Cloud runs the Store through `ployz-sdk`, and its
//! container log keeps stderr (#3786).
//!
//! A line carries no value it wasn't built to: its message only when it is one of
//! [`MESSAGES`], fixed text, so none formatted from a value passes; the fields
//! [`TEXT`] names, only as plain text or a number; and [`FORMATTED`]'s, row IDs.
//! Everything else reads [`REDACTED`], and the line says where it was logged. Each
//! text keeps its first line only, at most [`LONGEST`] bytes, so a database error's
//! `DETAIL` line, which can quote a row, never reaches it. What a named field holds is
//! the call site's to keep plain: each logs an error's message and code, never its
//! details or causes.

use std::io::Write;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// What filters the lines instead of [`DEFAULT`], in `EnvFilter` syntax.
pub const FILTER: &str = "PLOYZ_SDK_LOG";

/// Warnings and errors from the Store, and from the Deploys Cloud runs through it.
const DEFAULT: &str = "ployz_store=warn,ployz=warn,ployz_core=warn,ployz_sdk=warn";

/// The messages a line keeps: each a warning's fixed text.
const MESSAGES: &[&str] = &[
    "couldn't seal values below a Branch",
    "couldn't seal values a Branch inherited; it takes them at a later deploy",
    "couldn't delete a removed Volume's data",
    "couldn't record what the Deploy did to ingress",
    "couldn't record a Deploy warning; retrying later",
    "couldn't read the entry Machine's store; new gens follow the live answers only",
    "the entry Machine's store didn't answer in time; new gens follow the live answers only",
    "couldn't remove a failed start's Container",
];

/// Fields kept as the plain text or number they were logged with.
const TEXT: &[&str] = &[
    "branch",
    "code",
    "docker_volume",
    "environment",
    "error",
    "machine",
    "volume",
];

/// Fields kept formatted: row IDs, which name a row and never hold its value.
const FORMATTED: &[&str] = &["rows"];

const REDACTED: &str = "[redacted]";

/// The most of one text a line keeps.
const LONGEST: usize = 512;

/// Write each Store warning to stderr from now on, unless this process already has
/// a global subscriber. Installs once, however often it's called.
pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let directives = std::env::var(FILTER).ok();
        let subscriber =
            tracing_subscriber::registry().with(lines(std::io::stderr, filter(directives)));
        // A host's own subscriber wins.
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// `directives`, from [`FILTER`], when they're set and parse, else [`DEFAULT`]: an
/// empty one would let nothing through.
fn filter(directives: Option<String>) -> EnvFilter {
    directives
        .filter(|directives| !directives.trim().is_empty())
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT))
}

/// One JSON line per event `filter` lets through, written to `writer`.
fn lines<S, W>(writer: W, filter: EnvFilter) -> impl Layer<S>
where
    S: Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    Lines { writer }.with_filter(filter)
}

struct Lines<W> {
    writer: W,
}

impl<S, W> Layer<S> for Lines<W>
where
    S: Subscriber,
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let meta = event.metadata();
        let line = json!({
            "time_unix_ms": u64::try_from(at).unwrap_or(u64::MAX),
            "level": meta.level().as_str(),
            "target": meta.target(),
            "at": format!("{}:{}", meta.file().unwrap_or_default(), meta.line().unwrap_or_default()),
            "message": fields.message.unwrap_or_default(),
            "fields": fields.kept,
        });
        let mut text = line.to_string();
        text.push('\n');
        // Nowhere to report a failed write: the line is lost, as stderr's would be.
        let _ = self.writer.make_writer().write_all(text.as_bytes());
    }
}

/// An event's message and the fields a line keeps.
#[derive(Default)]
struct Fields {
    message: Option<String>,
    kept: Map<String, Value>,
}

impl Fields {
    fn keep(&mut self, field: &Field, value: Value) {
        self.kept.insert(field.name().to_owned(), value);
    }

    fn message(&mut self, message: String) {
        let known = MESSAGES.contains(&message.as_str());
        self.message = Some(if known { message } else { REDACTED.to_owned() });
    }

    fn number(&mut self, field: &Field, value: Value) {
        let value = if TEXT.contains(&field.name()) {
            value
        } else {
            Value::from(REDACTED)
        };
        self.keep(field, value);
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            return self.message(value.to_owned());
        }
        let value = if TEXT.contains(&field.name()) {
            first_line(value)
        } else {
            REDACTED.to_owned()
        };
        self.keep(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.number(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.number(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.number(field, Value::from(value));
    }

    /// `?` and `%` fields, an error's, and a formatted message: tracing records them
    /// alike.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message(format!("{value:?}"));
        } else if FORMATTED.contains(&field.name()) {
            self.keep(field, Value::from(first_line(&format!("{value:?}"))));
        } else {
            self.keep(field, Value::from(REDACTED));
        }
    }
}

/// `text`'s first line, at most [`LONGEST`] bytes of it.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let mut end = line.len().min(LONGEST);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.get(..end).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// What a test's lines wrote.
    #[derive(Clone, Default)]
    struct Written(Arc<Mutex<Vec<u8>>>);

    impl Write for Written {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Written {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    /// The lines `log` writes through the default filter, each parsed.
    fn logged(log: impl FnOnce()) -> Vec<Value> {
        let written = Written::default();
        let subscriber =
            tracing_subscriber::registry().with(lines(written.clone(), EnvFilter::new(DEFAULT)));
        tracing::subscriber::with_default(subscriber, log);
        let bytes = written
            .0
            .lock()
            .map(|bytes| bytes.clone())
            .unwrap_or_default();
        String::from_utf8(bytes)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect()
    }

    fn one(log: impl FnOnce()) -> Value {
        let lines = logged(log);
        assert_eq!(lines.len(), 1, "{lines:?}");
        lines.into_iter().next().unwrap_or_default()
    }

    #[test]
    fn a_warning_is_one_json_line_with_its_level_target_message_and_fields() {
        let line = one(|| {
            tracing::warn!(
                target: "ployz_store::branch::pair",
                branch = "00000000-0000-4000-8000-00000000000a",
                code = "unavailable",
                error = "canceling statement due to lock timeout",
                "couldn't seal values a Branch inherited; it takes them at a later deploy"
            );
        });
        assert_eq!(line.get("level"), Some(&json!("WARN")));
        assert_eq!(
            line.get("target"),
            Some(&json!("ployz_store::branch::pair"))
        );
        assert_eq!(
            line.get("message"),
            Some(&json!(
                "couldn't seal values a Branch inherited; it takes them at a later deploy"
            ))
        );
        assert_eq!(
            line.get("fields"),
            Some(&json!({
                "branch": "00000000-0000-4000-8000-00000000000a",
                "code": "unavailable",
                "error": "canceling statement due to lock timeout",
            }))
        );
    }

    #[test]
    fn only_the_fields_it_names_keep_their_text() {
        let line = one(|| {
            tracing::warn!(target: "ployz_store", volume = "pg-data", value = "fake-value-3786", "couldn't delete a removed Volume's data");
        });
        assert_eq!(
            line.get("fields"),
            Some(&json!({ "volume": "pg-data", "value": "[redacted]" }))
        );
    }

    #[test]
    fn a_formatted_field_is_redacted_except_row_ids() {
        let line = one(|| {
            tracing::warn!(
                target: "ployz_store",
                error = ?"fake-debug-3786",
                branch = %"fake-display-3786",
                rows = ?["web:variables.TOKEN"],
                "x"
            );
        });
        assert_eq!(
            line.get("fields"),
            Some(&json!({
                "error": "[redacted]",
                "branch": "[redacted]",
                "rows": "[\"web:variables.TOKEN\"]",
            }))
        );
    }

    #[test]
    fn each_text_keeps_its_first_line_only() {
        let line = one(|| {
            tracing::warn!(
                target: "ployz_store",
                error = "duplicate key value violates unique constraint\nDETAIL: Key (k)=(fake-row-3786) already exists.",
                "first\nsecond"
            );
        });
        assert_eq!(
            line.pointer("/fields/error"),
            Some(&json!("duplicate key value violates unique constraint"))
        );
        // Not one of the known messages, so it never shows, on either line.
        assert_eq!(line.get("message"), Some(&json!("[redacted]")));
        let long = "x".repeat(LONGEST + 10);
        let line = one(|| tracing::warn!(target: "ployz_store", error = long.as_str(), "x"));
        assert_eq!(
            line.pointer("/fields/error")
                .and_then(Value::as_str)
                .map(str::len),
            Some(LONGEST)
        );
    }

    #[test]
    fn only_warnings_from_the_store_and_what_it_runs_on_are_written() {
        let lines = logged(|| {
            tracing::info!(target: "ployz_store", "an info");
            tracing::warn!(target: "hyper", "another crate");
            tracing::warn!(target: "ployz::sdk::store_runner", "deploy");
            tracing::warn!(target: "ployz_core::rpc", "core");
            tracing::error!(target: "ployz_sdk", "sdk");
        });
        let targets: Vec<&Value> = lines.iter().filter_map(|line| line.get("target")).collect();
        assert_eq!(
            targets,
            [
                &json!("ployz::sdk::store_runner"),
                &json!("ployz_core::rpc"),
                &json!("ployz_sdk"),
            ]
        );
    }

    #[test]
    fn an_unset_empty_or_unparsable_filter_falls_back_to_warnings() {
        for directives in [None, Some(""), Some("  "), Some("ployz_store=nonsense")] {
            let filter = filter(directives.map(str::to_owned));
            assert_eq!(
                filter.to_string(),
                EnvFilter::new(DEFAULT).to_string(),
                "{directives:?}"
            );
        }
        assert_eq!(
            filter(Some("ployz_store=debug".to_owned())).to_string(),
            "ployz_store=debug"
        );
    }

    #[test]
    fn a_message_shows_only_when_it_is_a_known_text_and_the_line_says_where_it_was_logged() {
        let lines = logged(|| {
            tracing::warn!(target: "ployz_store", "couldn't delete a removed Volume's data");
            tracing::warn!(target: "ployz_store", "failed: {}", "plain-secret-3786");
            tracing::warn!(target: "ployz_store", message = ?"plain-secret-3786");
            tracing::warn!(target: "ployz_store", message = "plain-secret-3786");
        });
        let messages: Vec<&Value> = lines
            .iter()
            .filter_map(|line| line.get("message"))
            .collect();
        assert_eq!(
            messages,
            [
                &json!("couldn't delete a removed Volume's data"),
                &json!("[redacted]"),
                &json!("[redacted]"),
                &json!("[redacted]"),
            ]
        );
        let at = lines
            .first()
            .and_then(|line| line.get("at"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            at.starts_with("crates/ployz-store/src/stderr_log.rs:"),
            "{at}"
        );
    }

    /// A sealed value in any field of a warning never reaches the line, however it's
    /// logged: by a field the line doesn't name, formatted, or on a later line.
    #[test]
    fn a_sealed_value_in_a_warning_never_reaches_the_line() {
        let sealed = ployz_core::RpcError {
            code: ployz_core::RpcErrorCode::Conflict,
            message: "couldn't seal\nDETAIL: Key (value)=(plain-secret-3786)".to_owned(),
            details: json!({ "value": "plain-secret-3786", "sealed": "sealed-ciphertext-3786" }),
            cause: vec!["plain-secret-3786".to_owned()],
        };
        let lines = logged(|| {
            tracing::warn!(
                target: "ployz_store",
                value = "plain-secret-3786",
                secret = "sealed-ciphertext-3786",
                details = ?sealed.details,
                error = ?sealed,
                code = %sealed.code,
                "couldn't seal values below a Branch"
            );
            tracing::warn!(
                target: "ployz_store",
                error = sealed.message.as_str(),
                code = sealed.code.as_str(),
                "couldn't seal values below a Branch"
            );
        });
        assert_eq!(lines.len(), 2, "{lines:?}");
        let written = serde_json::to_string(&lines).unwrap_or_default();
        assert!(!written.contains("plain-secret-3786"), "{written}");
        assert!(!written.contains("sealed-ciphertext-3786"), "{written}");
        assert!(written.contains("couldn't seal"), "{written}");
    }
}
