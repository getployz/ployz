//! The one order every Store transaction takes row locks in, checked in debug and test
//! builds.
//!
//! Writers lock pull request rows, then Projects, then PR plans, then Environments, and
//! within each rank in key order. Two transactions that both keep to it never wait on
//! each other in a cycle, so they never deadlock on these rows.
//!
//! [`take`] names a row before the statement that locks it: an `UPDATE` or `DELETE` of
//! it, or an `INSERT ... DO UPDATE` that may meet it. A row taken before is taken again
//! at no cost. Taking a row that comes before the last one in the order panics, and so
//! does a write to one of these tables that no [`take`] named. A row no other
//! transaction can wait on with it ([`unordered`]) takes no place in the order: one the
//! transaction made itself, or one a migration writes while no writer runs beside it.
//! A lock taken with a bounded wait that gives up rather than wait on ([`bounded`])
//! can't close a cycle either, so it is held but not checked. Deleting a Project
//! deletes its plans and Environments by cascade, so it takes them first.
//!
//! Release builds check nothing.

use crate::id::{EnvironmentId, ProjectId, PullRequestNumber, RepositoryId};

/// A row whose lock has a place in the order: the variants are the ranks, first to last.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Lock {
    PullRequest {
        organization: String,
        repository: u64,
        number: u64,
    },
    Project(String),
    Plan {
        project: String,
        repository: u64,
    },
    Environment(String),
}

impl Lock {
    pub(crate) fn pull_request(
        organization: &str,
        repository: RepositoryId,
        number: PullRequestNumber,
    ) -> Self {
        Self::PullRequest {
            organization: organization.to_owned(),
            repository: repository.get(),
            number: number.get(),
        }
    }

    /// A pull request row as a migration reads it.
    pub(crate) fn pull_request_row(organization: &str, repository: i64, number: i64) -> Self {
        Self::PullRequest {
            organization: organization.to_owned(),
            repository: u64::try_from(repository).unwrap_or_default(),
            number: u64::try_from(number).unwrap_or_default(),
        }
    }

    pub(crate) fn project(id: &ProjectId) -> Self {
        Self::Project(id.as_str().to_owned())
    }

    pub(crate) fn plan(project: &ProjectId, repository: RepositoryId) -> Self {
        Self::Plan {
            project: project.as_str().to_owned(),
            repository: repository.get(),
        }
    }

    pub(crate) fn environment(id: &EnvironmentId) -> Self {
        Self::Environment(id.as_str().to_owned())
    }
}

/// Name `lock` before the statement that takes it. Panics, in debug and test builds,
/// when it comes before a lock this transaction already holds.
pub(crate) fn take(lock: Lock) {
    #[cfg(debug_assertions)]
    checked::take(lock, true);
    #[cfg(not(debug_assertions))]
    let _ = lock;
}

/// Name `lock`, a row no other transaction can wait on with this one: one it just
/// inserted, or one a migration writes.
pub(crate) fn unordered(lock: Lock) {
    #[cfg(debug_assertions)]
    checked::take(lock, false);
    #[cfg(not(debug_assertions))]
    let _ = lock;
}

/// `work`, whose locks wait a bounded time and give up rather than deadlock: held, but
/// free to come before the last lock. Locks after it are still checked against it.
pub(crate) fn bounded<T>(work: impl FnOnce() -> T) -> T {
    #[cfg(debug_assertions)]
    let _bounded = checked::Bounded::begin();
    work()
}

/// What a savepoint rolled back to holds again: the locks taken before it.
pub(crate) struct Mark {
    #[cfg(debug_assertions)]
    held: Option<checked::Held>,
}

/// The locks held now, for a savepoint that may roll back.
pub(crate) fn mark() -> Mark {
    Mark {
        #[cfg(debug_assertions)]
        held: checked::held(),
    }
}

/// A savepoint rolled back: the locks taken since `mark` are released.
pub(crate) fn rolled_back(mark: Mark) {
    #[cfg(debug_assertions)]
    checked::restore(mark.held);
    #[cfg(not(debug_assertions))]
    let _ = mark;
}

/// `work` in a transaction whose locks are checked, in debug and test builds.
#[cfg(debug_assertions)]
pub(super) fn checked<T>(
    work: impl FnOnce(&mut dyn super::Tx) -> Result<T, ployz_core::RpcError>,
) -> impl FnOnce(&mut dyn super::Tx) -> Result<T, ployz_core::RpcError> {
    move |tx| {
        let _scope = checked::Scope::begin();
        work(&mut checked::Checked(tx))
    }
}

#[cfg(not(debug_assertions))]
pub(super) const fn checked<T, W>(work: W) -> W
where
    W: FnOnce(&mut dyn super::Tx) -> Result<T, ployz_core::RpcError>,
{
    work
}

#[cfg(debug_assertions)]
mod checked {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use ployz_core::RpcError;

    use super::super::{Param, Row, Tx};
    use super::Lock;

    /// The tables whose rows have a place in the order.
    const RANKED: [&str; 4] = [
        "config_pull_request",
        "config_project",
        "config_pr_plan",
        "config_environment",
    ];

    #[derive(Clone, Debug, Default)]
    pub(super) struct Held {
        locks: BTreeSet<Lock>,
        /// The last lock [`take`] named, until the statement that takes it runs.
        named: Option<Lock>,
        /// Inside [`super::bounded`].
        bounded: bool,
    }

    thread_local! {
        static HELD: RefCell<Option<Held>> = const { RefCell::new(None) };
    }

    /// This thread's transaction, from its start to its end.
    pub(super) struct Scope(Option<Held>);

    impl Scope {
        pub(super) fn begin() -> Self {
            Self(HELD.replace(Some(Held::default())))
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            HELD.set(self.0.take());
        }
    }

    /// [`super::bounded`]'s span.
    pub(super) struct Bounded(bool);

    impl Bounded {
        pub(super) fn begin() -> Self {
            Self(HELD.with_borrow_mut(|held| {
                held.as_mut()
                    .is_some_and(|held| std::mem::replace(&mut held.bounded, true))
            }))
        }
    }

    impl Drop for Bounded {
        fn drop(&mut self) {
            HELD.with_borrow_mut(|held| {
                if let Some(held) = held {
                    held.bounded = self.0;
                }
            });
        }
    }

    pub(super) fn take(lock: Lock, ordered: bool) {
        HELD.with_borrow_mut(|held| {
            let Some(held) = held else {
                return;
            };
            if ordered
                && !held.bounded
                && !held.locks.contains(&lock)
                && let Some(last) = held.locks.last()
                && lock < *last
            {
                panic!("lock order: {lock:?} taken after {last:?}");
            }
            held.locks.insert(lock.clone());
            held.named = Some(lock);
        });
    }

    pub(super) fn held() -> Option<Held> {
        HELD.with_borrow(Clone::clone)
    }

    pub(super) fn restore(held: Option<Held>) {
        HELD.with_borrow_mut(|now| {
            if now.is_some() {
                *now = held;
            }
        });
    }

    /// The table a statement row-locks, if it is one of [`RANKED`].
    fn ranked(sql: &str) -> Option<&str> {
        let mut words = sql.split_whitespace();
        let first = words.next()?;
        let table = if first.eq_ignore_ascii_case("UPDATE") {
            words.next()?
        } else if first.eq_ignore_ascii_case("DELETE") || first.eq_ignore_ascii_case("INSERT") {
            let _ = words.next()?;
            let table = words.next()?;
            if first.eq_ignore_ascii_case("INSERT") && !sql.contains("DO UPDATE") {
                return None;
            }
            table
        } else {
            return None;
        };
        RANKED.into_iter().find(|ranked| *ranked == table)
    }

    /// Whether a statement on `table` with `params` reaches the row `lock` names: its
    /// table, with every part of its key among the parameters.
    fn reaches(lock: &Lock, table: &str, params: &[Param<'_>]) -> bool {
        let text = |value: &str| {
            params
                .iter()
                .any(|param| matches!(param, Param::Text(text) if *text == value))
        };
        let int = |value: u64| {
            params
                .iter()
                .any(|param| matches!(param, Param::Int(int) if u64::try_from(*int) == Ok(value)))
        };
        match lock {
            Lock::PullRequest {
                organization,
                repository,
                number,
            } => {
                table == "config_pull_request"
                    && text(organization)
                    && int(*repository)
                    && int(*number)
            }
            Lock::Project(id) => table == "config_project" && text(id),
            Lock::Plan {
                project,
                repository,
            } => table == "config_pr_plan" && text(project) && int(*repository),
            Lock::Environment(id) => table == "config_environment" && text(id),
        }
    }

    /// A write to a ranked table must follow the [`take`] that names its row.
    fn written(sql: &str, params: &[Param<'_>]) {
        let Some(table) = ranked(sql) else {
            return;
        };
        HELD.with_borrow_mut(|held| {
            let Some(held) = held else {
                return;
            };
            let named = held.named.take();
            assert!(
                named
                    .as_ref()
                    .is_some_and(|lock| reaches(lock, table, params)),
                "lock order: a write to {table} reaches no row named before it ({named:?}): \
                 route it through a lock helper\n{sql}"
            );
        });
    }

    pub(super) struct Checked<'a>(pub(super) &'a mut dyn Tx);

    impl Tx for Checked<'_> {
        fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError> {
            written(sql, params);
            self.0.execute(sql, params)
        }
        fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError> {
            written(sql, params);
            self.0.query(sql, params)
        }
        fn batch(&mut self, sql: &str) -> Result<(), RpcError> {
            self.0.batch(sql)
        }
        fn wait_for_locks(&mut self, at_most: Option<std::time::Duration>) -> Result<(), RpcError> {
            self.0.wait_for_locks(at_most)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{Lock, take};
        use super::{Scope, written};

        fn environment(id: &str) -> Lock {
            Lock::Environment(id.to_owned())
        }

        #[test]
        fn a_project_locked_after_one_of_its_environments_panics() {
            let _scope = Scope::begin();
            take(environment("b"));
            let inverted = std::panic::catch_unwind(|| take(Lock::Project("p".to_owned())));
            assert!(inverted.is_err());
        }

        #[test]
        fn environments_locked_out_of_key_order_panic_and_one_held_is_taken_again() {
            let _scope = Scope::begin();
            take(environment("a"));
            take(environment("c"));
            take(environment("a"));
            let inverted = std::panic::catch_unwind(|| take(environment("b")));
            assert!(inverted.is_err());
        }

        #[test]
        fn a_write_only_reaches_the_row_taken_before_it() {
            const SAVE: &str = "UPDATE config_environment SET working = ?1 WHERE id = ?2";
            let _scope = Scope::begin();
            take(environment("a"));
            written(SAVE, &["{}".into(), "a".into()]);
            take(environment("a"));
            let elsewhere = std::panic::catch_unwind(|| written(SAVE, &["{}".into(), "b".into()]));
            assert!(elsewhere.is_err());
            let unnamed = std::panic::catch_unwind(|| written(SAVE, &["{}".into(), "a".into()]));
            assert!(unnamed.is_err());
        }
    }
}
