use std::time::Duration;

use ployz_testkit::{hold_for, poll_until};

#[tokio::test(start_paused = true)]
async fn poll_until_returns_what_the_probe_produced() {
    let mut attempts = 0;
    let value = poll_until(Duration::from_secs(5), "third attempt", async || {
        attempts += 1;
        (attempts == 3).then_some(attempts)
    })
    .await
    .unwrap();
    assert_eq!(value, 3);
}

#[tokio::test(start_paused = true)]
async fn poll_until_names_the_last_observation_on_timeout() {
    let mut attempts = 0;
    let error = poll_until(Duration::from_secs(1), "never ready", async || {
        attempts += 1;
        Err::<(), _>(format!("attempt {attempts}"))
    })
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("never ready: not reached within 1s; last saw:\nattempt {attempts}")
    );
}

#[tokio::test(start_paused = true)]
async fn poll_until_bounds_a_probe_that_never_returns() {
    let error = poll_until(Duration::from_secs(1), "stuck probe", async || {
        std::future::pending::<bool>().await
    })
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "stuck probe: not reached within 1s");
}

#[tokio::test(start_paused = true)]
async fn hold_for_fails_on_the_first_broken_probe() {
    let mut attempts = 0;
    let error = hold_for(Duration::from_secs(5), "stays quiet", async || {
        attempts += 1;
        if attempts < 3 {
            Ok(())
        } else {
            Err(format!("broke on attempt {attempts}"))
        }
    })
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "stays quiet: broke after 500ms:\nbroke on attempt 3"
    );
}

#[tokio::test(start_paused = true)]
async fn hold_for_probes_until_the_window_ends() {
    let start = tokio::time::Instant::now();
    let mut last = start;
    hold_for(Duration::from_secs(1), "always holds", async || {
        last = tokio::time::Instant::now();
        true
    })
    .await
    .unwrap();
    assert_eq!(last - start, Duration::from_secs(1));
}
