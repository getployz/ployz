//! Run a test whose Deploy executes in-process, as the SDK and Cloud's runner run it.

/// A debug build's Deploy outgrows a test thread's 2 MiB once it executes, as it does the
/// CLI runner's thread, which gets the same 16 MiB.
const STACK: usize = 16 * 1024 * 1024;

/// Run `test` to completion on a current-thread runtime, on a thread with a 16 MiB stack.
/// Its panic is the test's.
pub fn run<F: Future<Output = ()>>(test: impl FnOnce() -> F + Send + 'static) {
    let outcome = std::thread::Builder::new()
        .name("big-stack-test".into())
        .stack_size(STACK)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(test());
        })
        .unwrap()
        .join();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
