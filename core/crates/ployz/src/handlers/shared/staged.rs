//! What a command that staged a change in Working State says, whichever noun it changed:
//! `Staged new Config app in shop/production.`, or `Nothing changed in shop/production; ...`
//! when the change was already so. A staged change carries the command to run next in
//! `--json`; the caller says it for a person, after any lines of its own.

use clap::ArgMatches;
use ployz_store::{
    ConfigStaged, DomainStaged, Edited, EnvironmentSummary, ServiceStaged, SettingPath,
    VolumeStaged,
};
use serde::Serialize;

use super::super::{Error, store};
use crate::ui::{self, Next};

/// A result that says what it staged, and for an edit what it applied at once.
pub(in crate::handlers) trait StagedResult: Serialize {
    fn environment(&self) -> &EnvironmentSummary;

    /// What waits for a Deploy; empty when the change was already so.
    fn staged(&self) -> &[SettingPath];

    /// What took effect at once.
    fn applied(&self) -> &[SettingPath] {
        &[]
    }
}

impl StagedResult for ConfigStaged {
    fn environment(&self) -> &EnvironmentSummary {
        &self.environment
    }

    fn staged(&self) -> &[SettingPath] {
        &self.staged
    }
}

impl StagedResult for ServiceStaged {
    fn environment(&self) -> &EnvironmentSummary {
        &self.environment
    }

    fn staged(&self) -> &[SettingPath] {
        &self.staged
    }
}

impl StagedResult for VolumeStaged {
    fn environment(&self) -> &EnvironmentSummary {
        &self.environment
    }

    fn staged(&self) -> &[SettingPath] {
        &self.staged
    }
}

impl StagedResult for DomainStaged {
    fn environment(&self) -> &EnvironmentSummary {
        &self.environment
    }

    fn staged(&self) -> &[SettingPath] {
        &self.staged
    }
}

impl StagedResult for Edited {
    fn environment(&self) -> &EnvironmentSummary {
        &self.environment
    }

    fn staged(&self) -> &[SettingPath] {
        &self.staged
    }

    fn applied(&self) -> &[SettingPath] {
        &self.immediate
    }
}

/// The command a staged change names next.
#[derive(Clone, Copy)]
pub(in crate::handlers) enum Then {
    /// `ployz diff`, to review it.
    Diff,
    /// `ployz deploy`, to ship it.
    Deploy,
}

impl Then {
    const fn words(self) -> &'static [&'static str] {
        match self {
            Self::Diff => &["diff"],
            Self::Deploy => &["deploy"],
        }
    }
}

/// One staged change as the command reports it. `V` is what `--json` prints.
pub(in crate::handlers) struct StagedReport<'a, V> {
    matches: &'a ArgMatches,
    value: &'a V,
    environment: &'a EnvironmentSummary,
    waiting: bool,
    nothing: bool,
    changed: String,
    unchanged: String,
    then: Then,
}

impl<'a, R: StagedResult> StagedReport<'a, R> {
    /// `changed` is the sentence's start, up to ` in PROJECT/ENVIRONMENT.`: its verb and
    /// noun. `unchanged` follows `Nothing changed in PROJECT/ENVIRONMENT; ` and says why.
    pub(in crate::handlers) fn of(
        matches: &'a ArgMatches,
        result: &'a R,
        changed: String,
        unchanged: String,
        then: Then,
    ) -> Self {
        let waiting = !result.staged().is_empty();
        Self {
            matches,
            value: result,
            environment: result.environment(),
            waiting,
            nothing: !waiting && result.applied().is_empty(),
            changed,
            unchanged,
            then,
        }
    }
}

impl<'a, V: Serialize> StagedReport<'a, V> {
    /// Print `value` under `--json` instead of the result this report reads.
    pub(in crate::handlers) fn shown<W: Serialize>(self, value: &'a W) -> StagedReport<'a, W> {
        StagedReport {
            matches: self.matches,
            value,
            environment: self.environment,
            waiting: self.waiting,
            nothing: self.nothing,
            changed: self.changed,
            unchanged: self.unchanged,
            then: self.then,
        }
    }

    /// The sentence, then `details` for a person, whether or not anything changed.
    ///
    /// # Errors
    ///
    /// Returns a serialization or stdout write error.
    pub(in crate::handlers) fn print(self, details: impl FnOnce()) -> Result<Next<'a, V>, Error> {
        let next = self
            .waiting
            .then(|| store::next(self.matches, self.then.words()));
        let followed = Next::new(self.value, next);
        ui::finish(&followed, || {
            // A mutation always says what it did, nothing included.
            if self.nothing {
                ui::stream(format_args!(
                    "Nothing changed in {}; {}.",
                    self.environment, self.unchanged
                ));
            } else if self.waiting {
                ui::stream(format_args!("{} in {}.", self.changed, self.environment));
            }
            details();
        })?;
        Ok(followed)
    }

    /// The sentence, then `details` for a person when something changed.
    ///
    /// # Errors
    ///
    /// Returns a serialization or stdout write error.
    pub(in crate::handlers) fn print_changes(
        self,
        details: impl FnOnce(),
    ) -> Result<Next<'a, V>, Error> {
        let nothing = self.nothing;
        self.print(|| {
            if !nothing {
                details();
            }
        })
    }
}
