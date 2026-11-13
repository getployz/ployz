//! Text the CLI reads from stdin or a path, never more than [`MAX_CLI_INPUT_BYTES`].
//!
//! A reader owns the wording of a refusal only where it is its own: the path it can't
//! open, or the bytes that aren't text. The bound and the unreadable stdin read the
//! same everywhere.

use std::{fs::File, io::Read};

use super::{Error, MAX_CLI_INPUT_BYTES, input_too_long, read_bounded};

/// Where an input comes from: stdin, or a path as typed.
#[derive(Clone, Copy)]
pub(crate) enum Source<'a> {
    Stdin,
    Path(&'a str),
}

impl<'a> Source<'a> {
    /// A `--from`-style argument, where `-` is stdin.
    pub(crate) fn of(arg: &'a str) -> Self {
        if arg == "-" {
            Self::Stdin
        } else {
            Self::Path(arg)
        }
    }

    /// How a refusal names it.
    pub(crate) fn name(self) -> &'a str {
        match self {
            Self::Stdin => "stdin",
            Self::Path(path) => path,
        }
    }
}

pub(crate) enum InputError<'a> {
    /// The path can't be opened or read, or stdin failed.
    Unreadable {
        source: Source<'a>,
        error: std::io::Error,
    },
    TooLong {
        source: Source<'a>,
        what: &'static str,
    },
    NotText {
        source: Source<'a>,
    },
}

/// What a reader that words no refusal of its own says: the read's own error for a
/// stdin that fails or isn't text, and the bound's refusal for an input past it.
impl From<InputError<'_>> for Error {
    fn from(error: InputError<'_>) -> Self {
        match error {
            InputError::Unreadable { error, .. } => error.into(),
            InputError::TooLong { source, what } => input_too_long(source.name(), what),
            InputError::NotText { .. } => std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
            .into(),
        }
    }
}

/// An input read whole and within the bound, not yet known to be text.
pub(crate) struct Input<'a> {
    bytes: Vec<u8>,
    source: Source<'a>,
}

impl<'a> Input<'a> {
    pub(crate) fn text(self) -> Result<String, InputError<'a>> {
        String::from_utf8(self.bytes).map_err(|_| InputError::NotText {
            source: self.source,
        })
    }
}

/// All of `reader`, which is `source`, or the refusal of what it holds past the
/// bound. `what` finishes "the CLI reads at most 32 MiB of …".
pub(crate) fn read_from<'a>(
    reader: impl Read,
    source: Source<'a>,
    what: &'static str,
) -> Result<Input<'a>, InputError<'a>> {
    match read_bounded(reader, MAX_CLI_INPUT_BYTES) {
        Ok(Some(bytes)) => Ok(Input { bytes, source }),
        Ok(None) => Err(InputError::TooLong { source, what }),
        Err(error) => Err(InputError::Unreadable { source, error }),
    }
}

pub(crate) fn read<'a>(
    source: Source<'a>,
    what: &'static str,
) -> Result<Input<'a>, InputError<'a>> {
    match source {
        Source::Stdin => read_from(std::io::stdin(), source, what),
        Source::Path(path) => match File::open(path) {
            Ok(file) => read_from(file, source, what),
            Err(error) => Err(InputError::Unreadable { source, error }),
        },
    }
}

/// `source` as text.
pub(crate) fn read_text<'a>(
    source: Source<'a>,
    what: &'static str,
) -> Result<String, InputError<'a>> {
    read(source, what)?.text()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dash_is_stdin_and_anything_else_is_a_path() {
        assert_eq!(Source::of("-").name(), "stdin");
        assert_eq!(Source::of("./-").name(), "./-");
        assert!(matches!(Source::of("-"), Source::Stdin));
    }

    #[test]
    fn text_within_the_bound_is_read_whole() {
        let text = read_from(&b"a: 1\n"[..], Source::Stdin, "a file")
            .and_then(Input::text)
            .ok();
        assert_eq!(text.as_deref(), Some("a: 1\n"));
    }

    #[test]
    fn an_input_past_the_bound_is_refused_by_the_bound_and_the_source() {
        let long = vec![b'a'; MAX_CLI_INPUT_BYTES + 1];
        let Err(refused) = read_from(&long[..], Source::Path("big.txt"), "a file") else {
            panic!("an input past the bound is refused");
        };
        let refused = Error::from(refused);
        assert_eq!(
            refused.to_string(),
            "big.txt: the CLI reads at most 32 MiB of a file; this one holds more"
        );
    }

    #[test]
    fn bytes_that_are_not_text_say_what_the_read_of_a_string_says() {
        let Err(refused) =
            read_from(&[0xff, 0xfe][..], Source::Stdin, "a file").and_then(Input::text)
        else {
            panic!("binary is not text");
        };
        assert_eq!(
            Error::from(refused).to_string(),
            "stream did not contain valid UTF-8"
        );
    }
}
