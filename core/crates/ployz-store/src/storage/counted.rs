//! How many statements a test's Store calls run, so a suite can hold work to a bound
//! that doesn't grow with the rows it passes over. Only the test Store counts.

#[cfg(not(feature = "test-support"))]
pub(super) const fn counted<T, W>(work: W) -> W
where
    W: FnOnce(&mut dyn super::Tx) -> Result<T, ployz_core::RpcError>,
{
    work
}

#[cfg(feature = "test-support")]
pub(super) use counting::counted;
#[cfg(feature = "test-support")]
pub(crate) use counting::statements;

#[cfg(feature = "test-support")]
mod counting {
    use std::cell::Cell;

    use ployz_core::RpcError;

    use super::super::{Param, Row, Tx};

    thread_local! {
        static STATEMENTS: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// `work`, counting its statements while [`statements`] runs on this thread.
    pub(in crate::storage) fn counted<T>(
        work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
    ) -> impl FnOnce(&mut dyn Tx) -> Result<T, RpcError> {
        move |tx| match STATEMENTS.get() {
            Some(_) => work(&mut Counted(tx)),
            None => work(tx),
        }
    }

    /// What `work` returns, and how many statements the Store calls it made ran.
    pub(crate) fn statements<T>(work: impl FnOnce() -> T) -> (T, usize) {
        STATEMENTS.set(Some(0));
        let done = work();
        (done, STATEMENTS.take().unwrap_or_default())
    }

    struct Counted<'a>(&'a mut dyn Tx);

    fn one() {
        STATEMENTS.set(STATEMENTS.get().map(|ran| ran + 1));
    }

    impl Tx for Counted<'_> {
        fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError> {
            one();
            self.0.execute(sql, params)
        }
        fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError> {
            one();
            self.0.query(sql, params)
        }
        fn batch(&mut self, sql: &str) -> Result<(), RpcError> {
            one();
            self.0.batch(sql)
        }
        fn wait_for_locks(&mut self, at_most: Option<std::time::Duration>) -> Result<(), RpcError> {
            self.0.wait_for_locks(at_most)
        }
    }
}
