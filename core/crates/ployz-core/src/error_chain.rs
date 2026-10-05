use std::error::Error;

/// Each `source()` below the top, raw, with a wrapper that only repeats the
/// line above it dropped.
#[must_use]
pub fn causes(top: &(dyn Error + 'static)) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut above = top.to_string();
    let mut next = top.source();
    while let Some(error) = next {
        let line = error.to_string();
        if line != above && !line.is_empty() {
            lines.push(line.clone());
        }
        above = line;
        next = error.source();
    }
    lines
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
}
