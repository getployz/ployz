//! Every byte a person sees goes through here: the mode, the palette, hints,
//! errors and exit codes. Handlers pass values and typed parts, never layout.

mod error;
mod exit;
mod mode;
mod tone;

#[cfg(test)]
pub(crate) use error::chain_text;
pub use error::{Hint, VALID_SHOWN, causes};
pub(crate) use error::{retrying, row, rpc_error, warn};
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
    /// The last `--color` in `args`, read before clap parses so clap's own
    /// errors and help obey it. Stops at `--` and at a trailing command such
    /// as `exec`'s.
    pub fn requested<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(
        command: &clap::Command,
        args: I,
    ) -> Self {
        let mut color = Self::Auto;
        let mut path = vec![command];
        let mut positionals = 0;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let current = path.last().copied().unwrap_or(command);
            let arg = arg.as_ref().to_str().unwrap_or("");
            if arg == "--" {
                break;
            }
            let option = |found: &dyn Fn(&clap::Arg) -> bool| {
                path.iter()
                    .rev()
                    .flat_map(|command| command.get_arguments())
                    .find(|arg| found(arg))
                    .is_some_and(|arg| arg.get_action().takes_values())
            };
            if let Some(long) = arg.strip_prefix("--") {
                let (name, inline) = long.split_once('=').unzip();
                let name = name.unwrap_or(long);
                if name == "color" {
                    let value = inline.map(str::to_owned).or_else(|| {
                        args.next()
                            .and_then(|value| value.as_ref().to_str().map(str::to_owned))
                    });
                    color = Self::named(value.as_deref().unwrap_or(""));
                } else if inline.is_none() && option(&|arg| arg.get_long() == Some(name)) {
                    args.next();
                }
            } else if let Some(shorts) = arg.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
                let valued = shorts
                    .char_indices()
                    .find(|(_, short)| option(&|arg| arg.get_short() == Some(*short)));
                if valued.is_some_and(|(at, short)| at + short.len_utf8() == shorts.len()) {
                    args.next();
                }
            } else if let Some(subcommand) = current.find_subcommand(arg) {
                path.push(subcommand);
                positionals = 0;
            } else {
                let trailing = current
                    .get_positionals()
                    .position(clap::Arg::is_trailing_var_arg_set);
                if trailing == Some(positionals) {
                    break;
                }
                positionals += 1;
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
    fn the_last_color_wins_anywhere_but_after_a_trailing_command() {
        let command = crate::cli::command();
        let color = |args: &[&str]| Color::requested(&command, args);
        assert_eq!(color(&["ps"]), Color::Auto);
        assert_eq!(color(&["--color", "never", "ps"]), Color::Never);
        assert_eq!(
            color(&["--color=never", "--color", "always", "ps"]),
            Color::Always
        );
        assert_eq!(
            color(&["--color=never", "ps", "--color", "always"]),
            Color::Always
        );
        assert_eq!(
            color(&["-c", "prod", "--color", "never", "deploy"]),
            Color::Never
        );
        assert_eq!(color(&["--context", "--color", "deploy"]), Color::Auto);
        assert_eq!(color(&["--ployz-config", "--color", "ps"]), Color::Auto);
        assert_eq!(color(&["server", "ls", "--color", "never"]), Color::Never);
        assert_eq!(
            color(&["exec", "web", "grep", "--color=always", "foo"]),
            Color::Auto
        );
        assert_eq!(
            color(&["exec", "--color", "never", "web", "grep", "--color=always"]),
            Color::Never
        );
        assert_eq!(color(&["ps", "--", "--color", "never"]), Color::Auto);
    }
}
