//! Every byte a person sees goes through here: the mode, the palette, hints,
//! errors and exit codes. Handlers pass values and typed parts, never layout.

mod error;
mod exit;
mod mode;
mod tone;

pub use error::{Hint, VALID_SHOWN, causes, inline};
pub use exit::{PARTIAL_EXIT, exit, exit_code};
pub use mode::{Mode, Surroundings, mode};
pub use tone::{Painted, Tone, clap_styles};

/// `--color`: when output carries color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// On a color terminal, unless `NO_COLOR`, `CLICOLOR=0` or `TERM=dumb` says no.
    Auto,
    Always,
    Never,
}

impl Color {
    pub fn before_subcommand<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(
        args: I,
    ) -> Self {
        let mut color = Self::Auto;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let Some(arg) = arg.as_ref().to_str().filter(|arg| arg.starts_with('-')) else {
                break;
            };
            let value = if arg == "--color" {
                args.next()
                    .and_then(|value| value.as_ref().to_str().map(str::to_owned))
            } else {
                arg.strip_prefix("--color=").map(str::to_owned)
            };
            if let Some(value) = value {
                color = Self::named(&value);
            }
        }
        color
    }

    #[must_use]
    pub fn named(value: &str) -> Self {
        match value {
            "always" => Self::Always,
            "never" => Self::Never,
            _ => Self::Auto,
        }
    }

    /// Make every `anstream` writer, clap's included, obey this choice.
    pub fn apply(self) {
        let choice = match self {
            Self::Auto => anstream::ColorChoice::Auto,
            Self::Always => anstream::ColorChoice::Always,
            Self::Never => anstream::ColorChoice::Never,
        };
        choice.write_global();
    }
}

/// Decide this run's mode from `--json` and where stderr goes.
pub fn init(json: bool) {
    mode::set(Mode::resolve(Surroundings::of_process(json)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_is_read_only_before_the_subcommand() {
        assert_eq!(Color::before_subcommand(["ps"]), Color::Auto);
        assert_eq!(
            Color::before_subcommand(["--color", "never", "ps"]),
            Color::Never
        );
        assert_eq!(
            Color::before_subcommand(["--color=never", "--color", "always", "ps"]),
            Color::Always
        );
        assert_eq!(
            Color::before_subcommand(["--color=never", "ps", "--color", "always"]),
            Color::Never
        );
        assert_eq!(
            Color::before_subcommand(["exec", "web", "grep", "--color=always", "foo"]),
            Color::Auto
        );
    }
}
