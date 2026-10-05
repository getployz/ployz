//! Every byte a person sees goes through here: the mode, the palette, hints,
//! errors and exit codes. Handlers pass values and typed parts, never layout.

mod error;
mod exit;
mod mode;
mod tone;

pub use error::{Hint, causes};
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
    /// The `--color` value in `args`, read before clap so its own errors obey it.
    /// The last one wins, as clap's would; a bad value is clap's to reject.
    pub fn from_args<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(args: I) -> Self {
        let mut color = Self::Auto;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let arg = arg.as_ref();
            if arg == "--" {
                break;
            }
            let value = if arg == "--color" {
                args.next().map(|value| value.as_ref().to_os_string())
            } else {
                arg.to_str()
                    .and_then(|arg| arg.strip_prefix("--color="))
                    .map(Into::into)
            };
            if let Some(value) = value.as_deref().and_then(std::ffi::OsStr::to_str) {
                color = match value {
                    "always" => Self::Always,
                    "never" => Self::Never,
                    _ => Self::Auto,
                };
            }
        }
        color
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
    fn color_is_read_before_clap_and_the_last_one_wins() {
        assert_eq!(Color::from_args(["ps"]), Color::Auto);
        assert_eq!(Color::from_args(["--color", "never", "ps"]), Color::Never);
        assert_eq!(
            Color::from_args(["--color=never", "ps", "--color", "always"]),
            Color::Always
        );
        assert_eq!(
            Color::from_args(["exec", "web", "--", "ls", "--color=always"]),
            Color::Auto
        );
    }
}
