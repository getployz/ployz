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
    async fn pointer_publisher(&mut self) -> Result<MachineTarget, RpcError> {
        Ok(MachineTarget::parse("publisher").unwrap())
    }

    async fn read_pointer_at(
        &mut self,
        _at: &mut MachineTarget,
        _service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        Ok(self.row.lock().unwrap().clone())
    }

    async fn publish_pointer_at(
        &mut self,
        _at: &mut MachineTarget,
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

pub(crate) fn deploy(deploy: char) -> DeploymentLogId {
    DeploymentLogId::parse(format!("dep-{deploy}")).unwrap()
}

const UNNAMED_SOURCE: char = '0';

pub(crate) fn pointer(deploy: char, containers: &[char], retiring: &[char]) -> ServingPointer {
    ServingPointer {
        deploy: self::deploy(deploy),
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

pub(crate) fn replacing(new: char, old: char) -> Rotation {
    Rotation {
        joining: vec![Join {
            copy: container(new),
            provenance: Provenance {
                source: container(old),
                source_deploy: None,
            },
        }],
        leaving: vec![container(old)],
        pruning: Vec::new(),
    }
}

pub(crate) fn labelled(new: char, deploy: char) -> Rotation {
    Rotation {
        joining: vec![Join {
            copy: container(new),
            provenance: Provenance {
                source: container(UNNAMED_SOURCE),
                source_deploy: Some(self::deploy(deploy)),
            },
        }],
        leaving: Vec::new(),
        pruning: Vec::new(),
    }
}

#[tokio::test]
async fn amending_a_service_with_no_pointer_writes_nothing() {
    let mut fake = FakeRow::default();
    let amended = amend_pointer(
        &mut fake,
        &service(),
        &deploy('a'),
        &CancellationToken::new(),
        &replacing('2', '1'),
    )
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
    let amended = amend_pointer(
        &mut fake,
        &service(),
        &deploy('a'),
        &CancellationToken::new(),
        &replacing('2', '1'),
    )
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
    let error = amend_pointer(
        &mut fake,
        &service(),
        &deploy('a'),
        &CancellationToken::new(),
        &labelled('2', 'a'),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ployz_core::RpcErrorCode::Conflict);
    assert_eq!(*fake.publishes.lock().unwrap(), AMEND_RETRIES + 1);
}

#[derive(Clone, Copy)]
enum Stall {
    Publisher,
    Read,
    Publish,
}

struct Stalled {
    row: FakeRow,
    stall: Stall,
}

impl PointerClient for Stalled {
    async fn pointer_publisher(&mut self) -> Result<MachineTarget, RpcError> {
        if matches!(self.stall, Stall::Publisher) {
            std::future::pending::<()>().await;
        }
        self.row.pointer_publisher().await
    }

    async fn read_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        if matches!(self.stall, Stall::Read) {
            std::future::pending::<()>().await;
        }
        self.row.read_pointer_at(at, service).await
    }

    async fn publish_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        if matches!(self.stall, Stall::Publish) {
            std::future::pending::<()>().await;
        }
        self.row.publish_pointer_at(at, request).await
    }
}

async fn cancelled_amendment(stall: Stall) -> (RpcError, FakeRow) {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointer('a', &['1'], &[]));
    let mut stalled = Stalled {
        row: row.clone(),
        stall,
    };
    let (service, cancellation) = (service(), CancellationToken::new());
    let (bound, rotation) = (deploy('a'), replacing('2', '1'));
    let amending = amend_pointer(&mut stalled, &service, &bound, &cancellation, &rotation);
    tokio::pin!(amending);
    tokio::select! {
        amended = &mut amending => panic!("returned before cancel: {amended:?}"),
        () = tokio::task::yield_now() => {}
    }
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), amending)
        .await
        .expect("cancellation returns promptly")
        .unwrap_err();
    assert_eq!(error.code, ployz_core::RpcErrorCode::Unavailable);
    (error, row)
}

#[tokio::test]
async fn cancelling_a_pending_publisher_lookup_or_pointer_read_publishes_nothing() {
    for stall in [Stall::Publisher, Stall::Read] {
        let (error, row) = cancelled_amendment(stall).await;
        assert!(error.message.contains("cancelled before"), "{error:?}");
        assert_eq!(*row.publishes.lock().unwrap(), 0);
        assert_eq!(*row.row.lock().unwrap(), Some(pointer('a', &['1'], &[])));
    }
}

#[tokio::test]
async fn cancelling_a_sent_publish_says_it_may_have_been_written() {
    let (error, _) = cancelled_amendment(Stall::Publish).await;
    assert_eq!(
        error.message,
        "cancelled after sending the Serving Pointer of app/api; it may have been written"
    );
}

#[test]
fn a_rotation_prunes_only_retiring_ids_the_listing_lacked_before_the_read() {
    let read = pointer('a', &['1'], &['8', '9']);
    let listed = [container('1'), container('2'), container('9')];
    let rotation = Rotation {
        joining: vec![Join {
            copy: container('2'),
            provenance: Provenance {
                source: container('1'),
                source_deploy: None,
            },
        }],
        leaving: vec![container('1')],
        pruning: absent_retiring(&read, &listed),
    };
    assert_eq!(rotation.pruning, [container('8')]);
    assert_eq!(
        rotation.apply(&read),
        Applied::Next(pointer('a', &['2'], &['9', '1'])),
        "8 is gone from the listing; 9 still runs and keeps retiring"
    );
    let later = pointer('a', &['1'], &['8', '9', '7']);
    assert_eq!(
        rotation.apply(&later),
        Applied::Next(pointer('a', &['2'], &['9', '7', '1'])),
        "7 retired after the listing; the listing proves nothing about it"
    );
    let prune_only = Rotation {
        pruning: vec![container('8'), container('9')],
        ..Rotation::default()
    };
    assert_eq!(
        prune_only.apply(&read),
        Applied::Next(pointer('a', &['1'], &[])),
        "pruning alone is a change worth publishing"
    );
}

#[test]
fn a_rotation_joins_with_evidence_retires_named_ids_and_reports_no_change() {
    let current = pointer('a', &['1', '2'], &['9']);
    assert_eq!(
        replacing('3', '1').apply(&current),
        Applied::Next(pointer('a', &['2', '3'], &['9', '1']))
    );
    assert_eq!(
        labelled('3', 'a').apply(&current),
        Applied::Next(pointer('a', &['1', '2', '3'], &['9'])),
        "a source labelled with the row's deploy is evidence without being named"
    );
    assert_eq!(
        Rotation::leaving([container('9')]).apply(&current),
        Applied::Unchanged
    );
    assert_eq!(
        Rotation::leaving([container('7')]).apply(&current),
        Applied::Unchanged,
        "an id the row does not name never enters retiring"
    );
    assert_eq!(
        replacing('2', '1').apply(&current),
        Applied::Next(pointer('a', &['2'], &['9', '1'])),
        "a copy already pointed joins without evidence"
    );
}

#[test]
fn a_join_without_evidence_is_refused() {
    let current = pointer('b', &['1'], &[]);
    assert_eq!(
        replacing('3', '7').apply(&current),
        Applied::Unproven,
        "7 is neither named nor labelled with dep-b"
    );
    assert_eq!(labelled('3', 'a').apply(&current), Applied::Unproven);
}

#[tokio::test]
async fn an_amendment_joining_without_evidence_is_outdated_and_writes_nothing() {
    let mut fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('b', &['1'], &[]));
    let amended = amend_pointer(
        &mut fake,
        &service(),
        &deploy('b'),
        &CancellationToken::new(),
        &labelled('3', 'a'),
    )
    .await
    .unwrap();
    assert_eq!(amended, Amendment::Outdated);
    assert_eq!(*fake.publishes.lock().unwrap(), 0);
    assert_eq!(*fake.row.lock().unwrap(), Some(pointer('b', &['1'], &[])));
}

#[tokio::test]
async fn an_amendment_under_an_older_deploy_is_outdated_even_with_evidence() {
    let mut fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('b', &['1'], &[]));
    let amended = amend_pointer(
        &mut fake,
        &service(),
        &deploy('a'),
        &CancellationToken::new(),
        &replacing('2', '1'),
    )
    .await
    .unwrap();
    assert_eq!(amended, Amendment::Outdated);
    assert_eq!(*fake.publishes.lock().unwrap(), 0);
    assert_eq!(*fake.row.lock().unwrap(), Some(pointer('b', &['1'], &[])));
}

#[tokio::test]
async fn an_amendment_losing_its_evidence_to_a_newer_row_is_outdated() {
    let mut fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('a', &['1'], &[]));
    fake.interlopers
        .lock()
        .unwrap()
        .push(pointer('a', &['4'], &['1']));
    let amended = amend_pointer(
        &mut fake,
        &service(),
        &deploy('a'),
        &CancellationToken::new(),
        &replacing('2', '1'),
    )
    .await
    .unwrap();
    assert_eq!(
        amended,
        Amendment::Outdated,
        "the winner retired the source; the copy's spec is unproven against the new row"
    );
    assert_eq!(
        *fake.row.lock().unwrap(),
        Some(pointer('a', &['4'], &['1']))
    );
}

#[tokio::test]
async fn a_read_immediately_before_removal_says_what_names_the_copy() {
    let mut fake = FakeRow::default();
    assert_eq!(
        protection(&mut fake, &service(), &container('1'))
            .await
            .unwrap(),
        Protection::Unnamed
    );
    let row = pointer('a', &['1'], &['2']);
    *fake.row.lock().unwrap() = Some(row.clone());
    assert_eq!(
        protection(&mut fake, &service(), &container('1'))
            .await
            .unwrap(),
        Protection::Pointed(row)
    );
    assert_eq!(
        protection(&mut fake, &service(), &container('2'))
            .await
            .unwrap(),
        Protection::Retiring
    );
    assert_eq!(
        protection(&mut fake, &service(), &container('3'))
            .await
            .unwrap(),
        Protection::Unnamed
    );
}

mod rpc {
    use std::sync::{Arc, Mutex};

    use ployz_core::{
        MachineList, MachineObservation, MembershipObservation, NotPointerPublisher, OpaquePayload,
        RpcRequestBody, RpcResponse, ServingPointerRead,
    };
    use tonic::{Request, Response};

    use super::*;
    use crate::global_fixtures::machine;

    struct Cluster {
        row: Arc<Mutex<Option<ServingPointer>>>,
        calls: Arc<Mutex<Vec<(&'static str, String)>>>,
    }

    async fn cluster(
        row: Option<ServingPointer>,
        down: Option<char>,
        refusing: Option<char>,
    ) -> (
        Client,
        Cluster,
        tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    ) {
        let row = Arc::new(Mutex::new(row));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fixture = Cluster {
            row: row.clone(),
            calls: calls.clone(),
        };
        let (client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let row = row.clone();
                let calls = calls.clone();
                let target = rpc
                    .metadata()
                    .get(ployz_core::ONE_TARGET_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
                    .unwrap_or_default();
                async move {
                    let body = rpc.into_inner().decode_request().unwrap().body;
                    let refusal = refusing
                        .filter(|refusing| target == machine(*refusing, "m").id.as_str())
                        .map(|refusing| NotPointerPublisher {
                            publisher: machine(if refusing == '1' { '2' } else { '1' }, "m").id,
                        });
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture answers only the pointer RPCs under test"
                    )]
                    let response = match body {
                        RpcRequestBody::DescribeContract(_) => {
                            RpcResponse::from(ployz_core::ContractDescription {
                                machine_id: machine('1', "m").id,
                                protocol_major: ployz_core::PROTOCOL_MAJOR,
                                daemon_version: "fixture".into(),
                                capabilities: Default::default(),
                            })
                        }
                        RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                            enrollment: None,
                            machines: ['1', '2']
                                .into_iter()
                                .filter(|hex| Some(*hex) != down)
                                .map(|hex| {
                                    MachineObservation::new(
                                        machine(hex, "m"),
                                        MembershipObservation::Up,
                                    )
                                })
                                .collect(),
                        }),
                        RpcRequestBody::ReadServingPointer(_) => {
                            calls.lock().unwrap().push(("read", target));
                            match refusal {
                                Some(refusal) => RpcResponse::from(refusal.rpc_error()),
                                None => RpcResponse::from(ServingPointerRead {
                                    pointer: row.lock().unwrap().clone(),
                                }),
                            }
                        }
                        RpcRequestBody::PublishServingPointer(request) => {
                            calls.lock().unwrap().push(("publish", target));
                            match refusal {
                                Some(refusal) => RpcResponse::from(refusal.rpc_error()),
                                None => {
                                    let mut row = row.lock().unwrap();
                                    RpcResponse::from(if *row == request.read {
                                        *row = Some(request.pointer);
                                        ServingPointerPublication::Published
                                    } else {
                                        ServingPointerPublication::Superseded {
                                            current: row.clone(),
                                        }
                                    })
                                }
                            }
                        }
                        other => panic!("unexpected pointer RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        (client, fixture, server)
    }

    fn at(hex: char) -> String {
        machine(hex, "m").id.to_string()
    }

    #[tokio::test]
    async fn a_superseded_publish_is_answered_once_and_never_resent() {
        let (mut client, fixture, server) =
            cluster(Some(pointer('b', &['2'], &[])), None, None).await;
        let publication = client
            .publish_serving_pointer(PublishServingPointerRequest {
                service: service(),
                read: Some(pointer('a', &['1'], &[])),
                pointer: pointer('a', &['3'], &[]),
            })
            .await
            .unwrap();
        assert_eq!(
            publication,
            ServingPointerPublication::Superseded {
                current: Some(pointer('b', &['2'], &[]))
            }
        );
        assert_eq!(*fixture.calls.lock().unwrap(), vec![("publish", at('1'))]);
        assert_eq!(
            *fixture.row.lock().unwrap(),
            Some(pointer('b', &['2'], &[]))
        );
        server.abort();
    }

    #[tokio::test]
    async fn an_amendment_reads_the_existing_row_and_publishes_over_it() {
        let (mut client, fixture, server) =
            cluster(Some(pointer('a', &['1'], &[])), None, None).await;
        let amended = amend_pointer(
            &mut client,
            &service(),
            &deploy('a'),
            &CancellationToken::new(),
            &replacing('2', '1'),
        )
        .await
        .unwrap();
        assert_eq!(amended, Amendment::Published { retries: 0 });
        assert_eq!(
            *fixture.row.lock().unwrap(),
            Some(pointer('a', &['2'], &['1']))
        );
        server.abort();
    }

    #[tokio::test]
    async fn an_amendment_follows_one_refusal_and_publishes_where_it_read() {
        let (mut client, fixture, server) =
            cluster(Some(pointer('a', &['1'], &[])), Some('1'), Some('2')).await;
        let amended = amend_pointer(
            &mut client,
            &service(),
            &deploy('a'),
            &CancellationToken::new(),
            &labelled('2', 'a'),
        )
        .await
        .unwrap();
        assert_eq!(amended, Amendment::Published { retries: 0 });
        assert_eq!(
            *fixture.calls.lock().unwrap(),
            vec![("read", at('2')), ("read", at('1')), ("publish", at('1'))]
        );
        server.abort();
    }
}
