use std::sync::{Arc, Mutex};

use ployz_core::{ContainerId, DeploymentLogId};

use super::*;

/// One shared row with the store's compare-and-set: write only over the value read.
#[derive(Clone, Default)]
pub(crate) struct FakeRow {
    pub(crate) row: Arc<Mutex<Option<ServingPointer>>>,
    pub(crate) publishes: Arc<Mutex<u32>>,
    /// Pointers another writer lands just before each of this client's publishes.
    pub(crate) interlopers: Arc<Mutex<Vec<ServingPointer>>>,
}

impl PointerClient for FakeRow {
    async fn read_pointer(
        &mut self,
        _service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        Ok(self.row.lock().unwrap().clone())
    }

    async fn publish_pointer(
        &mut self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        tokio::task::yield_now().await;
        if let Some(interloper) = self.interlopers.lock().unwrap().pop() {
            *self.row.lock().unwrap() = Some(interloper);
        }
        *self.publishes.lock().unwrap() += 1;
        let mut row = self.row.lock().unwrap();
        if *row != request.read {
            return Ok(ServingPointerPublication::Superseded {
                current: row.clone(),
            });
        }
        *row = Some(request.pointer);
        Ok(ServingPointerPublication::Published)
    }
}

pub(crate) fn pointer(deploy: char, containers: &[char], retiring: &[char]) -> ServingPointer {
    ServingPointer {
        deploy: DeploymentLogId::parse(format!("dep-{deploy}")).unwrap(),
        containers: containers.iter().map(|id| container(*id)).collect(),
        previous: None,
        retiring: retiring.iter().map(|id| container(*id)).collect(),
    }
}

pub(crate) fn container(id: char) -> ContainerId {
    ContainerId::parse(id.to_string().repeat(64)).unwrap()
}

fn service() -> QualifiedService {
    QualifiedService::parse("app/api").unwrap()
}

#[tokio::test]
async fn two_publishes_from_the_same_read_yield_exactly_one_published() {
    let fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('a', &['1'], &[]));
    let read = fake.row.lock().unwrap().clone();
    let publish = |next: ServingPointer| {
        let mut client = fake.clone();
        let read = read.clone();
        async move {
            client
                .publish_pointer(PublishServingPointerRequest {
                    service: service(),
                    read,
                    pointer: next,
                })
                .await
                .unwrap()
        }
    };
    let (first, second) = tokio::join!(
        publish(pointer('b', &['2'], &['1'])),
        publish(pointer('c', &['3'], &['1']))
    );
    let published = [&first, &second]
        .iter()
        .filter(|outcome| ***outcome == ServingPointerPublication::Published)
        .count();
    assert_eq!(published, 1, "{first:?} {second:?}");
    let winner = fake.row.lock().unwrap().clone();
    let loser = if first == ServingPointerPublication::Published {
        second
    } else {
        first
    };
    assert_eq!(
        loser,
        ServingPointerPublication::Superseded { current: winner }
    );
}

#[tokio::test]
async fn amending_a_service_with_no_pointer_writes_nothing() {
    let mut fake = FakeRow::default();
    let amended = amend_pointer(&mut fake, &service(), |current| {
        rotated(current, &[container('2')], &[])
    })
    .await
    .unwrap();
    assert_eq!(amended, Amendment::NoPointer);
    assert_eq!(*fake.publishes.lock().unwrap(), 0);
    assert_eq!(*fake.row.lock().unwrap(), None);
}

#[tokio::test]
async fn a_lost_amendment_retries_over_the_winner_and_keeps_both_changes() {
    let mut fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('a', &['1'], &[]));
    fake.interlopers
        .lock()
        .unwrap()
        .push(pointer('a', &['1', '4'], &[]));
    let amended = amend_pointer(&mut fake, &service(), |current| {
        rotated(current, &[container('2')], &[container('1')])
    })
    .await
    .unwrap();
    assert_eq!(amended, Amendment::Published { retries: 1 });
    assert_eq!(
        *fake.row.lock().unwrap(),
        Some(pointer('a', &['4', '2'], &['1']))
    );
}

#[tokio::test]
async fn an_amendment_gives_up_after_three_lost_retries() {
    let mut fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('a', &['1'], &[]));
    *fake.interlopers.lock().unwrap() = ['5', '6', '7', '8']
        .iter()
        .map(|id| pointer('a', &[*id], &[]))
        .collect();
    let error = amend_pointer(&mut fake, &service(), |current| {
        rotated(current, &[container('2')], &[])
    })
    .await
    .unwrap_err();
    assert_eq!(error.code, ployz_core::RpcErrorCode::Conflict);
    assert_eq!(*fake.publishes.lock().unwrap(), AMEND_RETRIES + 1);
}

#[test]
fn rotation_joins_retires_and_reports_no_change() {
    let current = pointer('a', &['1', '2'], &['9']);
    assert_eq!(
        rotated(&current, &[container('3')], &[container('1')]),
        Some(pointer('a', &['2', '3'], &['9', '1']))
    );
    assert_eq!(
        rotated(&current, &[container('2')], &[container('9')]),
        None
    );
}
