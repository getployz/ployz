use std::error::Error;

use crate::RpcError;

/// Each `source()` below the top, raw, with a wrapper that only repeats the
/// line above it dropped. An [`RpcError`] contributes the causes its peer sent.
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
            if line != above && !line.is_empty() {
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
}
