//! Structured logging wiring for daemon role processes.
//!
//! Role processes run under a supervisor that captures stderr into the
//! journal, so log lines go to stderr: JSON when stderr is not a terminal,
//! a human-readable format when it is. The `PLOYZ_LOG` environment variable
//! carries `tracing_subscriber::EnvFilter` directives; the default level is
//! `info`. Log lines are evidence for diagnosing the daemon itself —
//! operation status and events remain the product-facing record, and
//! nothing may read cluster truth back out of a log line.
//!
//! Attribution invariants:
//!
//! - Every log line names its role: a `process` field on JSON lines, a
//!   `[process]` prefix on terminal lines. One OS process runs exactly one
//!   role, so the name is stamped once at the subscriber layer and no code
//!   path — including `tokio::spawn`ed tasks, which do not inherit spans —
//!   can lose it.
//! - Span fields (an operation's `kind` and `operation_id`) reach every
//!   event inside the span even under a restrictive `PLOYZ_LOG` filter:
//!   the filter gates events only, spans are always enabled.
//! - `timestamp_ms`, `level`, `target`, and `process` are reserved keys;
//!   an event or span field with one of those names is overwritten.

use std::io::{IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::roles::DaemonProcessRole;
use tracing::field::{Field, Visit};
use tracing_subscriber::filter::FilterExt;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Environment variable holding the `EnvFilter` directive string.
pub const LOG_FILTER_ENV: &str = "PLOYZ_LOG";

const DEFAULT_FILTER: &str = "info";

/// The `process` value stamped on every log line of a role process.
#[must_use]
pub fn role_process_name(role: &DaemonProcessRole) -> &'static str {
    match role {
        DaemonProcessRole::Control => "control",
        DaemonProcessRole::Machine(_) => "machine",
        DaemonProcessRole::Gateway => "gateway",
        DaemonProcessRole::Dns => "dns",
    }
}

/// A `PLOYZ_LOG` value that could not be parsed as filter directives.
struct RejectedLogFilter {
    value: String,
    error: String,
}

/// Reads `PLOYZ_LOG`, distinguishing unset (default silently) from set but
/// malformed (default plus a rejection to report) — a swallowed typo would
/// otherwise leave an operator collecting `info` logs while believing a
/// `debug` directive is active.
fn read_log_filter() -> (EnvFilter, Option<RejectedLogFilter>) {
    let Ok(value) = std::env::var(LOG_FILTER_ENV) else {
        return (EnvFilter::new(DEFAULT_FILTER), None);
    };
    match EnvFilter::try_new(&value) {
        Ok(filter) => (filter, None),
        Err(error) => (
            EnvFilter::new(DEFAULT_FILTER),
            Some(RejectedLogFilter {
                value,
                error: error.to_string(),
            }),
        ),
    }
}

/// Installs the process-global subscriber for a daemon role process.
///
/// A caller that already installed a subscriber (an embedding test harness)
/// keeps its subscriber; this function never panics over registration.
pub fn init_daemon_logging(process: &'static str) {
    let (event_filter, rejected) = read_log_filter();
    // The filter gates events only; spans stay enabled so their fields
    // (operation kind and id) attribute the warn/error events that survive
    // a restrictive filter.
    let filter = event_filter.or(tracing_subscriber::filter::filter_fn(
        |metadata: &tracing::Metadata<'_>| metadata.is_span(),
    ));
    let registration = if std::io::stderr().is_terminal() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .event_format(ProcessPrefixFormat {
                        process,
                        inner: tracing_subscriber::fmt::format(),
                    })
                    .with_writer(std::io::stderr)
                    .with_filter(filter),
            )
            .try_init()
    } else {
        tracing_subscriber::registry()
            .with(
                ProcessJsonLayer {
                    process,
                    make_writer: std::io::stderr,
                }
                .with_filter(filter),
            )
            .try_init()
    };
    drop(registration);
    if let Some(rejected) = rejected {
        tracing::warn!(
            value = %rejected.value,
            error = %rejected.error,
            default = DEFAULT_FILTER,
            "PLOYZ_LOG was rejected; the default filter is in effect"
        );
    }
}

/// Prefixes every human-readable event with `[process]` — the terminal
/// counterpart of the JSON layer's constant `process` field, so role
/// attribution does not depend on the output format.
struct ProcessPrefixFormat<F> {
    process: &'static str,
    inner: F,
}

impl<S, N, F> tracing_subscriber::fmt::FormatEvent<S, N> for ProcessPrefixFormat<F>
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> tracing_subscriber::fmt::FormatFields<'writer> + 'static,
    F: tracing_subscriber::fmt::FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        write!(writer, "[{}] ", self.process)?;
        self.inner.format_event(ctx, writer, event)
    }
}

/// One JSON object per event on its own line: reserved metadata keys, the
/// constant `process`, span-scope fields (inner span wins), then event
/// fields (event wins over spans).
struct ProcessJsonLayer<W> {
    process: &'static str,
    make_writer: W,
}

/// Span fields captured at creation and stored in span extensions, so
/// events inside the span can inherit them at emit time.
struct SpanFields(serde_json::Map<String, serde_json::Value>);

impl<S, W> Layer<S> for ProcessJsonLayer<W>
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
    W: for<'writer> MakeWriter<'writer> + 'static,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = serde_json::Map::new();
        attrs.record(&mut JsonVisitor(&mut fields));
        span.extensions_mut().insert(SpanFields(fields));
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(SpanFields(fields)) = extensions.get_mut::<SpanFields>() else {
            return;
        };
        values.record(&mut JsonVisitor(fields));
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let mut object = serde_json::Map::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                let Some(SpanFields(fields)) = extensions.get::<SpanFields>() else {
                    continue;
                };
                for (key, value) in fields {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
        event.record(&mut JsonVisitor(&mut object));
        let metadata = event.metadata();
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        object.insert("timestamp_ms".to_owned(), timestamp_ms.into());
        object.insert("level".to_owned(), metadata.level().as_str().into());
        object.insert("target".to_owned(), metadata.target().into());
        object.insert("process".to_owned(), self.process.into());
        let mut line = serde_json::to_string(&object).unwrap_or_default();
        line.push('\n');
        let _ = self.make_writer.make_writer().write_all(line.as_bytes());
    }
}

/// Records event and span fields into a JSON map. The `message` field
/// arrives through `record_debug` as `fmt::Arguments`, whose `Debug`
/// output is its display text.
struct JsonVisitor<'map>(&'map mut serde_json::Map<String, serde_json::Value>);

impl Visit for JsonVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(
            field.name().to_owned(),
            serde_json::Number::from_f64(value)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
        );
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.into());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.0
            .insert(field.name().to_owned(), value.to_string().into());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_owned(), format!("{value:?}").into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Instrument;

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn lines(&self) -> Vec<serde_json::Value> {
            let buffer = self.0.lock().expect("buffer lock is not poisoned");
            String::from_utf8(buffer.clone())
                .expect("log output is UTF-8")
                .lines()
                .map(|line| serde_json::from_str(line).expect("log line is JSON"))
                .collect()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("buffer lock is not poisoned")
                .extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for SharedBuffer {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    fn subscriber_with(
        buffer: &SharedBuffer,
        directives: &str,
    ) -> impl tracing::Subscriber + Send + Sync {
        let filter = EnvFilter::new(directives).or(tracing_subscriber::filter::filter_fn(
            |metadata: &tracing::Metadata<'_>| metadata.is_span(),
        ));
        tracing_subscriber::registry().with(
            ProcessJsonLayer {
                process: "control",
                make_writer: buffer.clone(),
            }
            .with_filter(filter),
        )
    }

    #[test]
    fn events_carry_process_level_target_and_fields() {
        let buffer = SharedBuffer::default();
        tracing::subscriber::with_default(subscriber_with(&buffer, "info"), || {
            tracing::warn!(phase = "refresh", attempt = 3u64, "refresh failed");
        });
        let [line] = &buffer.lines()[..] else {
            panic!("expected exactly one log line");
        };
        assert_eq!(line["process"], "control");
        assert_eq!(line["level"], "WARN");
        assert_eq!(line["message"], "refresh failed");
        assert_eq!(line["phase"], "refresh");
        assert_eq!(line["attempt"], 3);
        assert!(line["timestamp_ms"].as_u64().is_some());
        assert!(line["target"].as_str().is_some());
    }

    #[tokio::test]
    async fn span_fields_survive_a_warn_filter_and_spawn_boundaries_do_not_lose_process() {
        let buffer = SharedBuffer::default();
        let _guard = tracing::subscriber::set_default(subscriber_with(&buffer, "warn"));
        // An info-level span under a warn filter: the span must still
        // attribute the error event emitted inside it.
        let in_span = async {
            tracing::info!("filtered out");
            tracing::error!("terminal transition could not be recorded");
        }
        .instrument(tracing::info_span!(
            "operation",
            kind = "deploy",
            operation_id = "op-1"
        ));
        in_span.await;
        // A bare spawn detaches from any span; the constant process field
        // must still be present.
        tokio::spawn(async {
            tracing::warn!("spawned diagnostic");
        })
        .await
        .expect("spawned task completes");
        let [error_line, spawned_line] = &buffer.lines()[..] else {
            panic!("expected exactly two log lines");
        };
        assert_eq!(error_line["kind"], "deploy");
        assert_eq!(error_line["operation_id"], "op-1");
        assert_eq!(error_line["process"], "control");
        assert_eq!(spawned_line["process"], "control");
        assert_eq!(spawned_line["message"], "spawned diagnostic");
    }

    #[test]
    fn terminal_lines_carry_the_process_prefix() {
        let buffer = SharedBuffer::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .event_format(ProcessPrefixFormat {
                    process: "gateway",
                    inner: tracing_subscriber::fmt::format(),
                })
                .with_ansi(false)
                .with_writer(buffer.clone()),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("terminal event");
        });
        let text = {
            let bytes = buffer.0.lock().expect("buffer lock is not poisoned");
            String::from_utf8(bytes.clone()).expect("log output is UTF-8")
        };
        let [line] = &text.lines().collect::<Vec<_>>()[..] else {
            panic!("expected exactly one log line");
        };
        assert!(
            line.starts_with("[gateway] "),
            "missing process prefix: {line}"
        );
        assert!(line.contains("terminal event"));
    }

    #[test]
    fn role_process_names_are_stable() {
        use ployz_core::ids::MachineId;
        let machine_id = MachineId::try_new("m-test".to_owned()).expect("valid machine id");
        assert_eq!(role_process_name(&DaemonProcessRole::Control), "control");
        assert_eq!(
            role_process_name(&DaemonProcessRole::Machine(machine_id)),
            "machine"
        );
        assert_eq!(role_process_name(&DaemonProcessRole::Gateway), "gateway");
        assert_eq!(role_process_name(&DaemonProcessRole::Dns), "dns");
    }
}
