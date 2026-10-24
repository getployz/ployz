//! BuildKit `rawjson` progress: one solve status per line, parsed into
//! structured steps so every consumer sees the same tree Buildx renders.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::Duration,
};

use base64::Engine as _;
use serde::Deserialize;

use crate::{BuildError, BuildStep, Progress};

/// Longest unterminated line held back before it is passed on as plain
/// output. A first vertex report can list every step of a large graph.
const MAX_LINE: usize = 16 * 1024 * 1024;

/// Longest unterminated step output held back before it is passed on.
const MAX_PARTIAL: usize = 64 * 1024;

#[derive(Deserialize)]
struct SolveStatus {
    #[serde(default)]
    vertexes: Vec<Vertex>,
    #[serde(default)]
    logs: Vec<VertexLog>,
}

#[derive(Deserialize)]
struct Vertex {
    digest: String,
    #[serde(default)]
    name: String,
    started: Option<String>,
    completed: Option<String>,
    #[serde(default)]
    cached: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
struct VertexLog {
    vertex: String,
    stream: u8,
    data: String,
}

/// Which step's stream a chunk of output belongs to.
#[derive(Clone, Eq, Hash, PartialEq)]
struct StreamKey {
    step: String,
    stderr: bool,
}

/// Splits Buildx output into lines and turns each solve status into progress.
/// Lines that are not solve statuses (warnings, plugin errors) stay `Output`.
/// Sealed Builds hide unreadable JSON fragments and oversized lines.
/// Step output is emitted in whole lines: BuildKit splits log records at
/// arbitrary byte offsets, even inside a multibyte character.
#[derive(Default)]
pub(crate) struct SolveParser {
    sealed: bool,
    discarding_line: bool,
    pending: Vec<u8>,
    partial: HashMap<StreamKey, Vec<u8>>,
    started: HashSet<String>,
    failed: Option<BuildError>,
}

impl SolveParser {
    pub(crate) fn new(sealed: bool) -> Self {
        Self {
            sealed,
            ..Self::default()
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8], progress: &dyn Fn(Progress)) {
        let bytes = if self.discarding_line {
            let Some(end) = bytes.iter().position(|byte| *byte == b'\n') else {
                return;
            };
            self.discarding_line = false;
            bytes.split_at(end + 1).1
        } else {
            bytes
        };
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<u8>>();
            self.emit_line(&line, progress);
        }
        if self.pending.len() > MAX_LINE {
            if self.sealed {
                self.pending.clear();
                self.discarding_line = true;
                Self::hidden_line(progress);
            } else {
                progress(Progress::Output(std::mem::take(&mut self.pending)));
            }
        }
    }

    /// The first step BuildKit reported failing for its own reason. The steps
    /// it cancelled because of that failure can report theirs first.
    pub(crate) fn failure(&mut self) -> Option<BuildError> {
        self.failed.take()
    }

    pub(crate) fn finish(&mut self, progress: &dyn Fn(Progress)) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            if self.sealed {
                self.emit_line(&line, progress);
            } else {
                progress(Progress::Output(line));
            }
        }
        for (key, bytes) in std::mem::take(&mut self.partial) {
            emit_output(key, &bytes, progress);
        }
    }

    fn emit_line(&mut self, line: &[u8], progress: &dyn Fn(Progress)) {
        if self.sealed && line.len() > MAX_LINE {
            Self::hidden_line(progress);
            return;
        }
        let Ok(status) = serde_json::from_slice::<SolveStatus>(line) else {
            if self.sealed
                && line
                    .iter()
                    .any(|byte| matches!(byte, b'{' | b'}' | b'[' | b']' | b'"'))
            {
                Self::hidden_line(progress);
            } else {
                progress(Progress::Output(line.to_vec()));
            }
            return;
        };
        // Register steps before their first output, but publish completion only
        // after the status's final logs. Repeated completions must not reopen rows.
        let mut completed = Vec::new();
        for vertex in status.vertexes {
            if let Some(error) = &vertex.error
                && self.failed.as_ref().is_none_or(|failed| {
                    matches!(failed, BuildError::Step { error, .. } if error.ends_with("context canceled"))
                })
            {
                self.failed = Some(BuildError::Step {
                    step: vertex.name.clone(),
                    error: error.clone(),
                });
            }
            let step = BuildStep {
                id: vertex.digest,
                name: vertex.name,
                started: vertex.started,
                completed: vertex.completed,
                cached: vertex.cached,
                error: vertex.error,
            };
            let first_start = step.started.is_some() && self.started.insert(step.id.clone());
            if step.completed.is_none() {
                progress(Progress::Step(step));
            } else {
                if first_start {
                    progress(Progress::Step(BuildStep {
                        completed: None,
                        cached: false,
                        error: None,
                        ..step.clone()
                    }));
                }
                completed.push(step);
            }
        }
        for log in status.logs {
            let key = StreamKey {
                step: log.vertex,
                stderr: log.stream == 2,
            };
            let mut bytes = self.partial.remove(&key).unwrap_or_default();
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&log.data)
                    .unwrap_or_default(),
            );
            let end = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|end| end + 1);
            // A stream with no newline (a progress bar, a minified artifact)
            // must not be held forever; pass it on at a character boundary.
            let end = end.or_else(|| (bytes.len() > MAX_PARTIAL).then(|| char_boundary(&bytes)));
            let rest = match end {
                Some(end) => bytes.split_off(end),
                None => std::mem::take(&mut bytes),
            };
            if !bytes.is_empty() {
                emit_output(key.clone(), &bytes, progress);
            }
            if !rest.is_empty() {
                self.partial.insert(key, rest);
            }
        }
        for step in completed {
            for stderr in [false, true] {
                let key = StreamKey {
                    step: step.id.clone(),
                    stderr,
                };
                if let Some(bytes) = self.partial.remove(&key) {
                    emit_output(key, &bytes, progress);
                }
            }
            progress(Progress::Step(step));
        }
    }

    fn hidden_line(progress: &dyn Fn(Progress)) {
        progress(Progress::Output(b"[build progress line hidden]\n".to_vec()));
    }
}

/// Length of the longest prefix that ends on a UTF-8 character boundary. An
/// invalid byte earlier on is already broken; only the trailing sequence matters.
fn char_boundary(bytes: &[u8]) -> usize {
    bytes.len() - ployz_core::scrub::incomplete_tail(bytes)
}

fn emit_output(key: StreamKey, bytes: &[u8], progress: &dyn Fn(Progress)) {
    progress(Progress::StepOutput {
        step: key.step,
        stderr: key.stderr,
        text: String::from_utf8_lossy(bytes).into_owned(),
    });
}

/// Renders structured progress the way `--progress=plain` would, for terminals.
#[derive(Default)]
pub struct PlainRenderer {
    steps: Mutex<HashMap<String, Seen>>,
}

/// Step number, first start, and last reported completion, so repeated
/// reports of one step print each transition once.
struct Seen {
    number: usize,
    started: Option<String>,
    completed: Option<String>,
}

impl PlainRenderer {
    /// Lines to print for this event, if any.
    #[must_use]
    pub fn render(&self, event: &Progress) -> Option<String> {
        let mut steps = self
            .steps
            .lock()
            .expect("rendering never panics while holding the step table");
        match event {
            Progress::Step(step) => {
                let count = steps.len() + 1;
                let Seen {
                    number,
                    started,
                    completed,
                } = steps.entry(step.id.clone()).or_insert_with(|| Seen {
                    number: count,
                    started: None,
                    completed: None,
                });
                let mut lines = String::new();
                if started.is_none() && step.started.is_some() {
                    *started = step.started.clone();
                    lines.push_str(&format!("#{number} {}\n", step.name));
                }
                if step.completed.is_some() && *completed != step.completed {
                    *completed = step.completed.clone();
                    if let Some(error) = &step.error {
                        lines.push_str(&format!("#{number} ERROR: {error}\n"));
                    } else if step.cached {
                        lines.push_str(&format!("#{number} CACHED\n"));
                    } else if let (Some(started), Some(completed)) =
                        (&step.started, &step.completed)
                    {
                        lines.push_str(&format!(
                            "#{number} DONE {:.1}s\n",
                            elapsed(started, completed).as_secs_f64()
                        ));
                    }
                }
                (!lines.is_empty()).then_some(lines)
            }
            Progress::StepOutput { step, text, .. } => {
                let number = steps.get(step).map_or(0, |seen| seen.number);
                Some(
                    text.lines()
                        .map(|line| format!("#{number} {line}\n"))
                        .collect(),
                )
            }
            Progress::Output(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            Progress::Stage(_) | Progress::Timing { .. } | Progress::Target { .. } => None,
        }
    }
}

fn elapsed(started: &str, completed: &str) -> Duration {
    let parse = chrono::DateTime::parse_from_rfc3339;
    match (parse(started), parse(completed)) {
        (Ok(started), Ok(completed)) => (completed - started).to_std().unwrap_or_default(),
        _ => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(feed: impl FnOnce(&mut SolveParser, &dyn Fn(Progress))) -> Vec<Progress> {
        let mut parser = SolveParser::default();
        let events = Mutex::new(Vec::new());
        let progress = |event| events.lock().unwrap().push(event);
        feed(&mut parser, &progress);
        parser.finish(&progress);
        events.into_inner().unwrap()
    }

    fn record(data: &[u8]) -> String {
        format!(
            "{{\"logs\":[{{\"vertex\":\"sha256:a\",\"stream\":1,\"data\":\"{}\"}}]}}\n",
            base64::engine::general_purpose::STANDARD.encode(data)
        )
    }

    fn texts(events: &[Progress]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                Progress::StepOutput { text, .. } => Some(text.as_str()),
                Progress::Step(_)
                | Progress::Output(_)
                | Progress::Stage(_)
                | Progress::Timing { .. }
                | Progress::Target { .. } => None,
            })
            .collect()
    }

    fn output(events: &[Progress]) -> Vec<u8> {
        events
            .iter()
            .filter_map(|event| match event {
                Progress::Output(bytes) => Some(bytes.as_slice()),
                Progress::Stage(_)
                | Progress::Step(_)
                | Progress::StepOutput { .. }
                | Progress::Timing { .. }
                | Progress::Target { .. } => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    #[test]
    fn a_vertex_without_a_name_still_decodes_its_logs() {
        let line = b"{\"vertexes\":[{\"digest\":\"sha256:a\"}],\"logs\":[{\"vertex\":\"sha256:a\",\"stream\":1,\"data\":\"aGVsbG8K\"}]}\n";
        for sealed in [false, true] {
            let events = collect(|parser, progress| {
                parser.sealed = sealed;
                parser.feed(line, progress);
            });
            assert_eq!(texts(&events), ["hello\n"]);
            assert!(output(&events).is_empty());
        }
    }

    #[test]
    fn unreadable_status_objects_are_hidden_only_for_sealed_builds() {
        for key in ["vertexes", "logs", "statuses", "warnings"] {
            for ending in ["", "\n"] {
                for line in [
                    format!(r#" {{"{key}":false,"logs":false,"message":"private"}}{ending}"#),
                    format!(r#" {{"{key}": ["private"{ending}"#),
                ] {
                    for sealed in [false, true] {
                        let events = collect(|parser, progress| {
                            parser.sealed = sealed;
                            parser.feed(line.as_bytes(), progress);
                        });
                        let expected = if sealed {
                            b"[build progress line hidden]\n".as_slice()
                        } else {
                            line.as_bytes()
                        };
                        assert_eq!(output(&events), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn unreadable_status_fragments_are_hidden_only_for_sealed_builds() {
        for ending in ["", "\n"] {
            for (fragment, hidden) in [
                (r#"b3BlbmVkLXRva2VuLTAxMjM0NTY3ODk=","stream":1}]}"#, true),
                (r#"stray fragment {"logs":[{"data":"private""#, true),
                ("fragment {", true),
                ("fragment }", true),
                ("fragment [", true),
                ("fragment ]", true),
                ("fragment \"", true),
                ("WARNING: plain warning", false),
            ] {
                let line = format!("{fragment}{ending}");
                for sealed in [false, true] {
                    let events = collect(|parser, progress| {
                        parser.sealed = sealed;
                        parser.feed(line.as_bytes(), progress);
                    });
                    let expected = if sealed && hidden {
                        "[build progress line hidden]\n"
                    } else {
                        &line
                    };
                    assert_eq!(output(&events), expected.as_bytes());
                }
            }
        }
    }

    #[test]
    fn oversized_lines_and_their_later_chunks_are_hidden_only_for_sealed_builds() {
        let oversized = "x".repeat(MAX_LINE + 1);
        for sealed in [false, true] {
            for chunked in [false, true] {
                for ending in ["", "\nwarning: plain\n"] {
                    let line = format!("{oversized}private{ending}");
                    let events = collect(|parser, progress| {
                        parser.sealed = sealed;
                        if chunked {
                            parser.feed(oversized.as_bytes(), progress);
                            parser.feed(b"priv", progress);
                            parser.feed(format!("ate{ending}").as_bytes(), progress);
                        } else {
                            parser.feed(line.as_bytes(), progress);
                        }
                    });
                    let expected = if sealed {
                        format!(
                            "[build progress line hidden]\n{}",
                            ending.trim_start_matches('\n')
                        )
                    } else {
                        line
                    };
                    assert_eq!(output(&events), expected.as_bytes());
                }
            }
        }
    }

    #[test]
    fn sealed_builds_decode_unterminated_statuses_and_keep_plain_warnings() {
        let events = collect(|parser, progress| {
            parser.sealed = true;
            parser.feed(b"WARNING: not json\n", progress);
            parser.feed(record(b"hello\n").trim_end().as_bytes(), progress);
        });
        assert_eq!(texts(&events), ["hello\n"]);
        assert_eq!(output(&events), b"WARNING: not json\n");
    }

    #[test]
    fn a_step_cancelled_by_the_failure_does_not_hide_it() {
        let mut parser = SolveParser::default();
        for (digest, name, error) in [
            (
                "sha256:b",
                "copy /mise/installs",
                "context canceled: context canceled",
            ),
            (
                "sha256:a",
                "npm run build",
                "process \\\"npm run build\\\" did not complete successfully: exit code: 3",
            ),
            ("sha256:c", "copy /app", "context canceled"),
        ] {
            parser.feed(
                format!(
                    "{{\"vertexes\":[{{\"digest\":\"{digest}\",\"name\":\"{name}\",\"started\":\"2026-10-18T15:09:30Z\",\"completed\":\"2026-10-18T15:09:31Z\",\"error\":\"{error}\"}}]}}\n"
                )
                .as_bytes(),
                &|_| {},
            );
        }
        assert_eq!(
            parser
                .failure()
                .map(|failure| failure.to_string())
                .as_deref(),
            Some(
                r#"npm run build: process "npm run build" did not complete successfully: exit code: 3"#
            )
        );
    }

    #[test]
    fn output_is_emitted_in_whole_lines_even_when_records_split_characters() {
        let (head, tail) = "hello world\nerror: 🐴\nunfinished".as_bytes().split_at(20);
        let events = collect(|parser, progress| {
            parser.feed(record(head).as_bytes(), progress);
            parser.feed(record(tail).as_bytes(), progress);
        });
        assert_eq!(
            texts(&events),
            ["hello world\n", "error: 🐴\n", "unfinished"]
        );
    }

    #[test]
    fn a_stream_without_newlines_is_passed_on_in_bounded_chunks() {
        let half = "x".repeat(MAX_PARTIAL / 2 + 10);
        let mut parser = SolveParser::default();
        let events = Mutex::new(Vec::new());
        let progress = |event| events.lock().unwrap().push(event);
        parser.feed(record(half.as_bytes()).as_bytes(), &progress);
        assert!(events.lock().unwrap().is_empty(), "held until the bound");
        parser.feed(record(half.as_bytes()).as_bytes(), &progress);
        // Passed on before the build ends, in one piece.
        assert_eq!(texts(&events.lock().unwrap()), [half.repeat(2)]);
    }

    #[test]
    fn the_bounded_cut_never_splits_a_character_after_an_invalid_byte() {
        let mut bytes = vec![0xFF];
        bytes.extend(std::iter::repeat_n(b'x', MAX_PARTIAL));
        let (head, tail) = "🐴\n".as_bytes().split_at(2);
        bytes.extend_from_slice(head);
        let events = collect(|parser, progress| {
            parser.feed(record(&bytes).as_bytes(), progress);
            parser.feed(record(tail).as_bytes(), progress);
        });
        let text = texts(&events).concat();
        assert_eq!(text, format!("\u{FFFD}{}🐴\n", "x".repeat(MAX_PARTIAL)));
    }

    #[test]
    fn a_final_record_in_the_completing_status_still_forms_a_whole_line() {
        let status = concat!(
            r#"{"logs":[{"vertex":"sha256:a","stream":1,"data":"aGVs"}]}"#,
            "\n",
            r#"{"vertexes":[{"digest":"sha256:a","name":"[1/1] RUN x","started":"2026-09-22T21:09:06Z","completed":"2026-09-22T21:09:07Z"}],"logs":[{"vertex":"sha256:a","stream":1,"data":"bG8K"}]}"#,
            "\n",
        );
        let events = collect(|parser, progress| parser.feed(status.as_bytes(), progress));
        assert_eq!(texts(&events), ["hello\n"]);
        let renderer = PlainRenderer::default();
        let rendered: String = events
            .iter()
            .filter_map(|event| renderer.render(event))
            .collect();
        assert_eq!(rendered, "#1 [1/1] RUN x\n#1 hello\n#1 DONE 1.0s\n");
    }

    #[test]
    fn a_new_vertex_precedes_its_first_log_in_the_same_status() {
        let status = concat!(
            r#"{"vertexes":[{"digest":"sha256:a","name":"RUN x","started":"2026-09-22T21:09:06Z"}],"logs":[{"vertex":"sha256:a","stream":1,"data":"aGVsbG8K"}]}"#,
            "\n",
        );
        let events = collect(|parser, progress| parser.feed(status.as_bytes(), progress));
        let renderer = PlainRenderer::default();
        let rendered: String = events
            .iter()
            .filter_map(|event| renderer.render(event))
            .collect();
        assert_eq!(rendered, "#1 RUN x\n#1 hello\n");
    }

    #[test]
    fn parses_vertexes_and_logs_and_keeps_other_lines_as_output() {
        let status = concat!(
            r#"{"vertexes":[{"digest":"sha256:a","name":"[sdk 1/2] RUN cargo build","started":"2026-09-22T21:09:06.000000000Z"}]}"#,
            "\n",
            r#"{"logs":[{"vertex":"sha256:a","stream":1,"data":"aGVsbG8K","timestamp":"2026-09-22T21:09:07.000000000Z"}]}"#,
            "\nWARNING: not json\n",
            r#"{"vertexes":[{"digest":"sha256:a","name":"[sdk 1/2] RUN cargo build","started":"2026-09-22T21:09:06.000000000Z","completed":"2026-09-22T21:11:28.500000000Z"}]}"#,
            "\n",
            // BuildKit re-reports a finished vertex; the renderer prints DONE once.
            r#"{"vertexes":[{"digest":"sha256:a","name":"[sdk 1/2] RUN cargo build","started":"2026-09-22T21:09:06.000000000Z","completed":"2026-09-22T21:11:28.500000000Z"}]}"#,
            "\n",
        );
        // Split mid-line so reassembly is exercised.
        let (head, tail) = status.as_bytes().split_at(40);
        let events = collect(|parser, progress| {
            parser.feed(head, progress);
            parser.feed(tail, progress);
        });
        let mut seen = events.iter();
        assert!(
            matches!(seen.next(), Some(Progress::Step(step)) if step.id == "sha256:a" && step.completed.is_none())
        );
        assert!(
            matches!(seen.next(), Some(Progress::StepOutput { step, stderr: false, text }) if step == "sha256:a" && text == "hello\n")
        );
        assert!(
            matches!(seen.next(), Some(Progress::Output(bytes)) if bytes == b"WARNING: not json\n")
        );
        assert!(matches!(seen.next(), Some(Progress::Step(step)) if step.completed.is_some()));
        assert!(matches!(seen.next(), Some(Progress::Step(_))));
        assert!(seen.next().is_none());

        let renderer = PlainRenderer::default();
        let rendered: String = events
            .iter()
            .filter_map(|event| renderer.render(event))
            .collect();
        assert_eq!(
            rendered,
            "#1 [sdk 1/2] RUN cargo build\n#1 hello\nWARNING: not json\n#1 DONE 142.5s\n"
        );
    }
}
