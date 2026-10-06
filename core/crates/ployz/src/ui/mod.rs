//! Every byte a person sees goes through here: the mode, the palette, hints,
//! errors and exit codes. Handlers pass values and typed parts, never layout.

mod error;
mod exit;
mod mode;
mod result;
mod table;
mod tone;

#[cfg(test)]
pub(crate) use error::chain_text;
pub use error::{Hint, VALID_SHOWN, causes};
pub(crate) use error::{retrying, row, rpc_error, warn_cause};
#[cfg(test)]
pub(crate) use exit::plain;
pub use exit::{PARTIAL_EXIT, exit, exit_code};
pub use mode::{Mode, Surroundings, mode};
pub use result::json;
pub(crate) use result::{
    Gaps, captured, done, emit, emit_committed, emit_line, emitted, fields, finish, finish_fanout,
    hint, interactive, list, note, note_inline, show, stream, warn,
};
pub use table::{Cell, Fields, Table};
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
    /// as `exec`'s. Like clap, an option whose value is optional, such as
    /// `env sync --to`, never takes a following flag as its value.
    ///
    /// Only clap's own output depends on this scan: once clap has parsed,
    /// [`init`] applies the `--color` clap saw.
    pub fn requested<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(
        command: &clap::Command,
        args: I,
    ) -> Self {
        let mut color = Self::Auto;
        let mut path = vec![command];
        let mut positionals = 0;
        let mut args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_str().unwrap_or("").to_owned())
            .peekable();
        while let Some(arg) = args.next() {
            let current = path.last().copied().unwrap_or(command);
            if arg == "--" {
                break;
            }
            let option = |found: &dyn Fn(&clap::Arg) -> bool| {
                path.iter()
                    .rev()
                    .flat_map(|command| command.get_arguments())
                    .find(|arg| found(arg))
                    .filter(|arg| arg.get_action().takes_values())
                    .map(|arg| arg.get_num_args().is_some_and(|n| n.min_values() == 0))
            };
            let value_follows = |optional: bool, next: Option<&String>| {
                !optional || next.is_some_and(|next| !next.starts_with('-'))
            };
            if let Some(long) = arg.strip_prefix("--") {
                let (name, inline) = long.split_once('=').unzip();
                let name = name.unwrap_or(long);
                if name == "color" {
                    let value = inline.map(str::to_owned).or_else(|| args.next());
                    color = Self::named(value.as_deref().unwrap_or(""));
                } else if inline.is_none()
                    && option(&|arg| arg.get_long() == Some(name))
                        .is_some_and(|optional| value_follows(optional, args.peek()))
                {
                    args.next();
                }
            } else if let Some(shorts) = arg.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
                let valued = shorts.char_indices().find_map(|(at, short)| {
                    option(&|arg| arg.get_short() == Some(short))
                        .map(|optional| (at + short.len_utf8() == shorts.len(), optional))
                });
                if valued
                    .is_some_and(|(last, optional)| last && value_follows(optional, args.peek()))
                {
                    args.next();
                }
            } else if let Some(subcommand) = current.find_subcommand(&arg) {
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

    /// Make every `anstream` writer, clap's included, obey this choice in `mode`.
    pub fn apply(self, mode: Mode) {
        self.choice(mode, clicolor_force()).write_global();
    }

    /// Only Interactive output is colored on its own; Plain and Json carry
    /// color only when `--color always` or `CLICOLOR_FORCE` asks for it.
    /// anstream alone would also color when `CI` is set.
    const fn choice(self, mode: Mode, forced: bool) -> anstream::ColorChoice {
        match self {
            Self::Always => anstream::ColorChoice::Always,
            Self::Never => anstream::ColorChoice::Never,
            Self::Auto if forced || matches!(mode, Mode::Interactive) => {
                anstream::ColorChoice::Auto
            }
            Self::Auto => anstream::ColorChoice::Never,
        }
    }
}

fn clicolor_force() -> bool {
    std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| !value.is_empty() && value != "0")
}

/// Decide this run's mode from `--json` and where stderr goes, then color
/// from `--color` and that mode.
pub fn init(json: bool, color: Color) {
    let mode = Mode::resolve(Surroundings::of_process(json));
    mode::set(mode);
    color.apply(mode);
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

    #[test]
    fn an_optional_value_never_swallows_color_and_the_scan_agrees_with_clap() {
        let command = crate::cli::command();
        let cases: &[(&[&str], Color)] = &[
            (&["env", "sync", "--to", "--color", "never"], Color::Never),
            (
                &["env", "sync", "--to", "staging", "--color", "never"],
                Color::Never,
            ),
            (&["env", "sync", "--color", "never", "--to"], Color::Never),
            (
                &[
                    "volume",
                    "add",
                    "data",
                    "--shared-writes",
                    "--color",
                    "never",
                ],
                Color::Never,
            ),
            (
                &[
                    "volume",
                    "add",
                    "data",
                    "--color",
                    "always",
                    "--shared-writes",
                ],
                Color::Always,
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(Color::requested(&command, *args), *expected, "{args:?}");
            let parsed = command
                .clone()
                .try_get_matches_from(std::iter::once("ployz").chain(args.iter().copied()))
                .unwrap();
            let seen = parsed.get_one::<String>("color").unwrap();
            assert_eq!(Color::named(seen), *expected, "clap on {args:?}");
        }
    }

    #[test]
    fn only_interactive_output_is_colored_unless_asked() {
        use anstream::ColorChoice;
        assert_eq!(
            Color::Auto.choice(Mode::Interactive, false),
            ColorChoice::Auto
        );
        assert_eq!(Color::Auto.choice(Mode::Plain, false), ColorChoice::Never);
        assert_eq!(Color::Auto.choice(Mode::Json, false), ColorChoice::Never);
        assert_eq!(Color::Auto.choice(Mode::Plain, true), ColorChoice::Auto);
        assert_eq!(
            Color::Always.choice(Mode::Plain, false),
            ColorChoice::Always
        );
        assert_eq!(
            Color::Never.choice(Mode::Interactive, true),
            ColorChoice::Never
        );
    }
}
