use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::{RpcError, RpcErrorCode};
use tokio_util::sync::CancellationToken;

use super::{Answer, set_each};

async fn yields(times: usize) {
    for _ in 0..times {
        tokio::task::yield_now().await;
    }
}

fn set(answer: &Answer<usize>) -> Option<usize> {
    match answer {
        Answer::Set(value) => Some(**value),
        Answer::Refused(_) | Answer::Cancelled => None,
    }
}

// Catches a report that names the wrong Server for an answer: each answer sits at its
// Machine's place, however late it arrived.
#[tokio::test]
async fn answers_keep_the_machines_order_whichever_answers_first() {
    let finished = Mutex::new(Vec::new());
    let answers = set_each(
        &[0_usize, 1, 2],
        3,
        &CancellationToken::new(),
        |_| None,
        |machine| {
            let finished = &finished;
            async move {
                yields((2 - machine) * 3).await;
                finished.lock().unwrap().push(machine);
                Ok::<_, RpcError>(machine)
            }
        },
    )
    .await;
    assert_eq!(*finished.lock().unwrap(), [2, 1, 0]);
    assert_eq!(
        answers.iter().map(set).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
}

// Catches a command that starts every router or door at once, which a slow Server
// then holds against the rest.
#[tokio::test]
async fn no_more_than_at_once_are_asked_together() {
    let (asking, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let answers = set_each(
        &[0_usize, 1, 2, 3, 4],
        2,
        &CancellationToken::new(),
        |_| None,
        |machine| {
            let (asking, most) = (&asking, &most);
            async move {
                most.fetch_max(asking.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                yields(4).await;
                asking.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, RpcError>(machine)
            }
        },
    )
    .await;
    assert_eq!(most.load(Ordering::SeqCst), 2);
    assert_eq!(answers.iter().filter_map(set).count(), 5);
}

// Catches a cancelled command waiting on a Server that never answers, and one that
// calls a Server it already refused.
#[tokio::test]
async fn a_cancelled_command_stops_waiting_but_a_refusal_stays_a_refusal() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let asked = AtomicUsize::new(0);
    let answers = set_each(
        &[0_usize, 1],
        2,
        &cancel,
        |machine| (*machine == 0).then(|| RpcError::unavailable("too old".to_owned())),
        |_| {
            asked.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<Result<usize, RpcError>>()
        },
    )
    .await;
    let [Answer::Refused(refused), Answer::Cancelled] = answers.as_slice() else {
        panic!("a refusal then a cancel, got {answers:?}");
    };
    assert_eq!(refused.message, "too old");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the refused Server isn't called"
    );
}

// Catches one Server's failure ending the rest: each is answered by itself.
#[tokio::test]
async fn a_daemons_failure_is_its_machines_answer_alone() {
    let answers = set_each(
        &[0_usize, 1],
        1,
        &CancellationToken::new(),
        |_| None,
        |machine| async move {
            if machine == 0 {
                Err(RpcError::unavailable("down".to_owned()))
            } else {
                Ok(machine)
            }
        },
    )
    .await;
    let [Answer::Refused(failed), second] = answers.as_slice() else {
        panic!("the first Server's failure, then an answer, got {answers:?}");
    };
    assert_eq!(failed.code, RpcErrorCode::Unavailable);
    assert_eq!(set(second), Some(1));
}
