//! How this run renders, decided once before dispatch.

use std::sync::atomic::{AtomicU8, Ordering};

/// The rendering every output obeys for this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A person at a terminal: color, live redraw and prompts.
    Interactive,
    /// A pipe, a log or CI: the same lines, nothing redrawn, no prompts.
    Plain,
    /// `--json`: one object on stdout, nothing prompts.
    Json,
}

/// What the mode is decided from, read once from the process.
#[derive(Clone, Copy, Debug)]
pub struct Surroundings {
    pub json: bool,
    pub stderr_is_terminal: bool,
    pub term_is_dumb: bool,
    pub ci: bool,
}

impl Surroundings {
    #[must_use]
    pub fn of_process(json: bool) -> Self {
        use std::io::IsTerminal as _;
        Self {
            json,
            stderr_is_terminal: std::io::stderr().is_terminal(),
            term_is_dumb: std::env::var_os("TERM").is_some_and(|term| term == "dumb"),
            ci: std::env::var_os("CI").is_some(),
        }
    }
}

impl Mode {
    #[must_use]
    pub const fn resolve(around: Surroundings) -> Self {
        if around.json {
            Self::Json
        } else if around.stderr_is_terminal && !around.term_is_dumb && !around.ci {
            Self::Interactive
        } else {
            Self::Plain
        }
    }

    const fn to_u8(self) -> u8 {
        match self {
            Self::Interactive => 0,
            Self::Plain => 1,
            Self::Json => 2,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Interactive,
            2 => Self::Json,
            _ => Self::Plain,
        }
    }
}

// Process config: set before dispatch, read from any thread (the deploy renderer too).
static MODE: AtomicU8 = AtomicU8::new(Mode::Plain.to_u8());

pub(crate) fn set(mode: Mode) {
    MODE.store(mode.to_u8(), Ordering::Relaxed);
}

/// The mode this process renders in.
#[must_use]
pub fn mode() -> Mode {
    Mode::from_u8(MODE.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERSON: Surroundings = Surroundings {
        json: false,
        stderr_is_terminal: true,
        term_is_dumb: false,
        ci: false,
    };

    #[test]
    fn json_wins_then_a_real_terminal_is_interactive() {
        let resolve = Mode::resolve;
        assert_eq!(
            resolve(Surroundings {
                json: true,
                ..PERSON
            }),
            Mode::Json
        );
        assert_eq!(resolve(PERSON), Mode::Interactive);
        let piped = Surroundings {
            stderr_is_terminal: false,
            ..PERSON
        };
        assert_eq!(resolve(piped), Mode::Plain);
        let dumb = Surroundings {
            term_is_dumb: true,
            ..PERSON
        };
        assert_eq!(resolve(dumb), Mode::Plain);
        assert_eq!(resolve(Surroundings { ci: true, ..PERSON }), Mode::Plain);
    }
}
