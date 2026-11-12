use std::{error::Error, fmt};

use crate::RpcError;

/// Each `source()` below the top, raw, dropping a line the line above already
/// ends with. An [`RpcError`] contributes the causes its peer sent.
#[must_use]
pub fn causes(top: &(dyn Error + 'static)) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut above = top.to_string();
    let mut current = top;
    loop {
        let remote = current
            .downcast_ref::<RpcError>()
            .map_or(&[][..], |error| &error.cause[..]);
        let below = current.source();
        for line in remote.iter().cloned().chain(below.map(ToString::to_string)) {
            if !line.is_empty() && !repeats(&above, &line) {
                lines.push(line.clone());
            }
            above = line;
        }
        let Some(below) = below else {
            return lines;
        };
        current = below;
    }
}

fn repeats(above: &str, line: &str) -> bool {
    above
        .strip_suffix(line)
        .is_some_and(|head| head.is_empty() || head.ends_with(": "))
}

/// An error and its causes on one line, for a place no `cause:` line can go:
/// a warning, a per-Server failure, an RPC message.
#[must_use]
pub fn inline(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    for cause in causes(error) {
        text.truncate(text.trim_end_matches('.').len());
        text.push_str(": ");
        text.push_str(&cause);
    }
    text
}

/// A per-Server row, record or report field: [`inline`] with its lines folded onto one.
#[must_use]
pub fn row(error: &(dyn Error + 'static)) -> String {
    one_line(&inline(error))
}

/// Foreign text on one line: a registry's `denied\ndenied` would break the
/// indented line it lands in. Repeated lines print once.
#[must_use]
pub fn one_line(text: &str) -> String {
    joined(text.lines().map(str::trim))
}

/// `trimmed` lines without the empty ones and without a line that repeats the one
/// before, joined with `; `.
#[must_use]
pub fn joined<'a>(trimmed: impl Iterator<Item = &'a str>) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in trimmed.filter(|line| !line.is_empty()) {
        if lines.last() != Some(&line) {
            lines.push(line);
        }
    }
    lines.join("; ")
}

/// `error` as an [`RpcError`] of `code`, its message the [`row`] of the chain.
#[must_use]
pub fn rpc_error(code: crate::RpcErrorCode, error: &(dyn Error + 'static)) -> RpcError {
    RpcError::new(code, row(error))
}

/// `text` as a failure's first line: a sentence, ending with a period.
#[must_use]
pub fn sentence(mut text: String) -> String {
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

/// `next` follows `message` as its own sentence, with one break however
/// `message` ended. A dangling colon introduces nothing, so it becomes the break.
#[must_use]
pub fn then_sentence(message: &str, next: &str) -> String {
    match message.trim_end().trim_end_matches(':').trim_end() {
        "" => next.to_owned(),
        message => format!("{} {next}", sentence(message.to_owned())),
    }
}

/// `what`, then `: reason` when there is a reason to give.
#[must_use]
pub fn with_reason(what: impl std::fmt::Display, reason: &str) -> String {
    match reason.trim() {
        "" => what.to_string(),
        reason => format!("{what}: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Layer>>);

    impl std::fmt::Display for Layer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl Error for Layer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.1
                .as_deref()
                .map(|layer| layer as &(dyn Error + 'static))
        }
    }

    fn chain(lines: &[&'static str]) -> Layer {
        lines
            .iter()
            .rev()
            .fold(None, |below, line| Some(Layer(line, below.map(Box::new))))
            .unwrap()
    }

    #[test]
    fn a_cause_that_repeats_the_line_above_is_skipped() {
        let error = chain(&[
            "Could not deploy.",
            "disk full",
            "disk full",
            "",
            "os error 28",
        ]);
        assert_eq!(causes(&error), ["disk full", "os error 28"]);
    }

    #[test]
    fn a_cause_the_line_above_already_ends_with_is_skipped() {
        let error = chain(&[
            "Docker operation failed",
            "Error in the hyper legacy client: client error (Connect)",
            "client error (Connect)",
            "Connection refused (os error 111)",
        ]);
        assert_eq!(
            causes(&error),
            [
                "Error in the hyper legacy client: client error (Connect)",
                "Connection refused (os error 111)"
            ]
        );
    }

    #[test]
    fn inline_joins_the_chain_and_drops_each_trailing_period() {
        let error = chain(&["Could not deploy.", "Volume is full.", "os error 28"]);
        assert_eq!(
            inline(&error),
            "Could not deploy: Volume is full: os error 28"
        );
        assert_eq!(inline(&chain(&["Nothing below."])), "Nothing below.");
    }

    #[test]
    fn an_rpc_error_lists_the_causes_its_peer_sent() {
        let error = RpcError {
            code: crate::RpcErrorCode::Internal,
            message: "Could not create the Container.".into(),
            details: serde_json::Value::Null,
            cause: vec![
                "Docker responded with status code 500".into(),
                "denied".into(),
            ],
        };
        assert_eq!(
            causes(&error),
            ["Docker responded with status code 500", "denied"]
        );
        assert_eq!(
            inline(&error),
            "Could not create the Container: Docker responded with status code 500: denied"
        );
    }

    #[test]
    fn a_foreign_line_end_trims_off_the_one_line_form() {
        let error = std::io::Error::other("refused a.txt\r\n\tdenied \r\n");
        assert_eq!(row(&error), "refused a.txt; denied");
        let wire = serde_json::to_value(rpc_error(crate::RpcErrorCode::Internal, &error));
        assert_eq!(
            wire.unwrap().pointer("/message").unwrap(),
            "refused a.txt; denied"
        );
    }

    #[test]
    fn a_failure_sentence_ends_with_one_period() {
        assert_eq!(sentence("Could not deploy".to_owned()), "Could not deploy.");
        assert_eq!(
            sentence("Could not deploy.".to_owned()),
            "Could not deploy."
        );
        assert_eq!(sentence("Really?".to_owned()), "Really?");
        assert_eq!(sentence("Stop!".to_owned()), "Stop!");
    }

    #[test]
    fn the_next_sentence_follows_with_one_break_however_the_message_ended() {
        assert_eq!(
            then_sentence("Could not deploy", "Run it again."),
            "Could not deploy. Run it again."
        );
        assert_eq!(
            then_sentence("Could not deploy.  ", "Run it again."),
            "Could not deploy. Run it again."
        );
        assert_eq!(
            then_sentence("Could not deploy:", "Run it again."),
            "Could not deploy. Run it again."
        );
        assert_eq!(then_sentence(" : ", "Run it again."), "Run it again.");
    }

    #[test]
    fn a_reason_follows_its_subject_only_when_there_is_one() {
        assert_eq!(
            with_reason("Volume web", " no space "),
            "Volume web: no space"
        );
        assert_eq!(with_reason("Volume web", "  "), "Volume web");
    }
}

/// Error lines kept as a source chain, when the errors themselves can't be kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinedChain {
    line: String,
    next: Option<Box<JoinedChain>>,
}

impl JoinedChain {
    /// A chain of `lines`, outermost first; `None` when there are none.
    #[must_use]
    pub fn of(lines: Vec<String>) -> Option<Self> {
        lines.into_iter().rev().fold(None, |next, line| {
            Some(Self {
                line,
                next: next.map(Box::new),
            })
        })
    }
}

impl fmt::Display for JoinedChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.line)
    }
}

impl Error for JoinedChain {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.next
            .as_deref()
            .map(|next| next as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod joined_chain_tests {
    use super::*;

    #[test]
    fn lines_become_a_chain_outermost_first() {
        let chain = JoinedChain::of(vec!["outer".into(), "middle".into(), "inner".into()]).unwrap();
        assert_eq!(chain.to_string(), "outer");
        assert_eq!(causes(&chain), ["middle", "inner"]);
        assert!(JoinedChain::of(Vec::new()).is_none());
    }
}
