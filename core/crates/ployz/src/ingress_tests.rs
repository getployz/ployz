use ployz_core::{
    ContainerRuntimeObservation, Machine, MembershipObservation, Placement, RpcErrorCode,
};

use super::*;
use crate::global_fixtures::{container_id, global_service, machine, running_on};

fn observed(machine: Machine) -> MachineObservation {
    MachineObservation::new(machine, MembershipObservation::Up)
}

fn door(hex: char, name: &str) -> DoorMachine {
    let machine = machine(hex, name);
    DoorMachine {
        id: machine.id,
        name: machine.name,
        supported: true,
    }
}

fn error(code: RpcErrorCode, message: &str) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

fn outcome(runtime: ContainerRuntimeObservation, change: DoorChange) -> DoorOutcome {
    let mut now = running_on(&machine('a', "one"), 'c');
    now.try_update(|parts| parts.runtime = runtime).unwrap();
    DoorOutcome { now, change }
}

fn running(change: DoorChange) -> DoorOutcome {
    outcome(
        ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::Healthy,
        },
        change,
    )
}

fn both(holder: DoorChange, proxy: DoorChange) -> IngressDoorsSet {
    IngressDoorsSet {
        holder: Some(running(holder)),
        proxy: Some(running(proxy)),
        ..IngressDoorsSet::default()
    }
}

fn answered(set: IngressDoorsSet) -> Answer {
    Answer::Set(Box::new(set))
}

fn stopped_at(stage: IngressStage, error: RpcError) -> IngressDoorsSet {
    IngressDoorsSet {
        holder: (stage == IngressStage::Proxy).then(|| running(DoorChange::Created)),
        failure: Some(IngressDoorsFailure { stage, error }),
        ..IngressDoorsSet::default()
    }
}

const PORT_TAKEN: &str = "failed to bind host port 0.0.0.0:80/tcp: address already in use";

#[test]
fn an_unchecked_ingress_names_the_server_and_why() {
    let silent = Failure::from(ConnectError::Rpc(
        crate::connect::TransportError::budget_expired("no reply"),
    ));
    assert_eq!(
        unchecked(&silent, Some("box-node-09c8".into()), None),
        "Server box-node-09c8 isn't answering (no reply)"
    );
    assert_eq!(unchecked(&silent, None, None), crate::ui::row(&silent));

    let answered = Failure::from(ConnectError::Remote(error(
        RpcErrorCode::Internal,
        "Docker is down",
    )));
    assert_eq!(
        unchecked(&answered, Some("box-node-09c8".into()), None),
        "Docker is down (on Server box-node-09c8)"
    );

    let local = Failure::usage("no Cluster selected");
    assert_eq!(
        unchecked(&local, Some("box-node-09c8".into()), None),
        crate::ui::row(&local)
    );

    let refused = Failure::from(ConnectError::AllFailed {
        relayed: false,
        selection: crate::context::ConnectionSource::Cloud,
        attempts: 1,
        server: Some("web-2".into()),
        retry: ployz_core::Retry::Never,
        reached: false,
        last: Some(Box::new(ConnectError::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused,
        )))),
    });
    assert_eq!(
        unchecked(&refused, Some("other".into()), None),
        "Server web-2 isn't answering (connection refused)"
    );

    let machine_id = MachineId::parse("1".repeat(32)).unwrap();
    let raw = Failure::from(ConnectError::AllFailed {
        relayed: false,
        selection: crate::context::ConnectionSource::Cloud,
        attempts: 1,
        server: Some(machine_id.to_string()),
        retry: ployz_core::Retry::Never,
        reached: false,
        last: Some(Box::new(ConnectError::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused,
        )))),
    });
    let machine_name = MachineName::parse("machine-1").unwrap();
    assert_eq!(
        unchecked(
            &raw,
            Some(machine_id.to_string()),
            Some((&machine_id, &machine_name))
        ),
        "Server machine-1 isn't answering (connection refused)"
    );
    assert_eq!(
        unchecked(&raw, Some(machine_id.to_string()), None),
        format!("Server {machine_id} isn't answering (connection refused)")
    );
    let id_shaped_name = "2".repeat(32);
    let other_name = MachineName::parse("machine-2").unwrap();
    assert_eq!(
        unchecked(
            &silent,
            Some(id_shaped_name.clone()),
            Some((&machine_id, &other_name)),
        ),
        format!("Server {id_shaped_name} isn't answering (no reply)")
    );
}

// Catches the Ingress Proxy being set on a Server without the role, one that is leaving
// or unreachable, or a Server with an older daemon being asked at all.
#[test]
fn only_reachable_staying_ingress_servers_get_doors() {
    let with_role = observed(machine('a', "one"));
    let mut without_role = observed(machine('b', "two"));
    without_role.machine.accepts_ingress = false;
    let mut leaving = observed(machine('c', "three"));
    leaving.machine.runtime.leaving = true;
    let down = MachineObservation::new(machine('d', "four"), MembershipObservation::Down);
    let mut older = observed(machine('e', "five"));
    older.machine.runtime.capabilities = ["ployz.something.else.v1".to_owned()].into();
    let mut newer = observed(machine('f', "six"));
    newer.machine.runtime.capabilities = [SET_INGRESS_DOORS_CAPABILITY.to_owned()].into();

    let doors = door_machines(&[with_role, without_role, leaving, down, older, newer]);

    assert_eq!(
        doors
            .iter()
            .map(|door| (door.name.to_string(), door.supported))
            .collect::<Vec<_>>(),
        [
            ("one".to_owned(), true),
            ("five".to_owned(), false),
            ("six".to_owned(), true),
        ]
    );
}

// Catches a command allocating a new proxy id while one is observed, which would make
// every Server replace its holder.
#[test]
fn the_newest_observed_proxy_is_the_one_every_server_follows() {
    let one = machine('a', "one");
    let ingress = |id, created_at, hex| {
        let mut copy = running_on(&one, hex);
        copy.try_update(|parts| parts.created_at_unix_nanos = created_at)
            .unwrap();
        global_service(
            QualifiedService::system_ingress(),
            id,
            Placement::default(),
            copy,
        )
    };
    let user = global_service(
        crate::global_fixtures::qualified("app", "api"),
        'f',
        Placement::default(),
        running_on(&one, 'e'),
    );

    let services = [ingress('1', 1, 'b'), user, ingress('2', 2, 'c')];

    assert_eq!(
        proxy_service(&services),
        crate::global_fixtures::service_id('2')
    );
    assert_ne!(proxy_service(&[]), proxy_service(&[]));
}

// Catches a holder that couldn't bind 80 or 443 reading as anything but a taken port, and
// a Caddy failure reading as one.
#[test]
fn only_the_holder_stage_finds_a_port_taken() {
    let mut held = error(RpcErrorCode::Internal, "Docker operation failed");
    held.cause = vec![PORT_TAKEN.into()];
    let Some(IngressFailure::PortTaken { cause }) =
        failure_of(&stopped_at(IngressStage::Holder, held.clone()))
    else {
        panic!("a holder that can't bind is a taken port");
    };
    assert!(cause.contains("address already in use"), "{cause}");

    assert!(matches!(
        failure_of(&stopped_at(IngressStage::Proxy, held)),
        Some(IngressFailure::Failed { .. })
    ));
    // InvalidArgument covers more than ports, so the code alone says nothing.
    assert!(matches!(
        failure_of(&stopped_at(
            IngressStage::Holder,
            error(RpcErrorCode::InvalidArgument, "invalid mount config")
        )),
        Some(IngressFailure::Failed { .. })
    ));
}

#[test]
fn a_caddy_that_isnt_running_says_how() {
    let restarting = IngressDoorsSet {
        holder: Some(running(DoorChange::Unchanged)),
        proxy: Some(outcome(
            ContainerRuntimeObservation::Restarting,
            DoorChange::Created,
        )),
        ..IngressDoorsSet::default()
    };
    let Some(IngressFailure::Failed { cause }) = failure_of(&restarting) else {
        panic!("a restarting Caddy failed");
    };
    assert!(cause.contains("keeps crashing"), "{cause}");
    assert!(
        cause.contains(&format!("docker logs {}", container_id('c'))),
        "{cause}"
    );

    let exited = IngressDoorsSet {
        holder: Some(running(DoorChange::Unchanged)),
        proxy: Some(outcome(
            ContainerRuntimeObservation::Exited { code: 1 },
            DoorChange::Created,
        )),
        ..IngressDoorsSet::default()
    };
    let Some(IngressFailure::Failed { cause }) = failure_of(&exited) else {
        panic!("an exited Caddy failed");
    };
    assert!(cause.contains("stopped right after starting"), "{cause}");

    let half = IngressDoorsSet {
        holder: Some(running(DoorChange::Created)),
        ..IngressDoorsSet::default()
    };
    assert!(matches!(
        failure_of(&half),
        Some(IngressFailure::Failed { cause }) if cause.contains("no Ingress Proxy Caddy")
    ));
    assert_eq!(
        failure_of(&both(DoorChange::Unchanged, DoorChange::Unchanged)),
        None
    );
}

// Catches a Deploy reporting a repair on a Server whose Ingress Proxy already ran, or
// missing one the daemon made.
#[test]
fn a_deploy_reports_what_each_daemon_did() {
    let one = door('a', "one");
    let repair = |action, failure| {
        Ok(Some(IngressRepair {
            machine: one.id,
            server: "one".into(),
            action,
            failure,
        }))
    };

    assert_eq!(
        checked(
            &one,
            answered(both(DoorChange::Unchanged, DoorChange::Unchanged))
        ),
        Ok(None)
    );
    assert_eq!(
        checked(
            &one,
            answered(both(DoorChange::Unchanged, DoorChange::Rejoined))
        ),
        repair(IngressAction::Start, None)
    );
    assert_eq!(
        checked(
            &one,
            answered(both(DoorChange::Started, DoorChange::Created))
        ),
        repair(IngressAction::Create, None)
    );
    assert!(matches!(
        checked(
            &one,
            answered(stopped_at(
                IngressStage::Holder,
                error(RpcErrorCode::Internal, PORT_TAKEN)
            ))
        ),
        Ok(Some(IngressRepair {
            failure: Some(IngressFailure::PortTaken { .. }),
            ..
        }))
    ));
    assert_eq!(
        checked(
            &one,
            Answer::Refused(error(RpcErrorCode::Internal, "Docker is down"))
        ),
        repair(
            IngressAction::Create,
            Some(IngressFailure::Failed {
                cause: "Docker is down".into()
            })
        )
    );
}

// Catches an older daemon, a silent Server or a cancelled Deploy being reported as a
// failed repair when nothing on that Server is known.
#[test]
fn a_server_nothing_is_known_about_is_unchecked() {
    let one = door('a', "one");

    let Err(older) = checked(&one, Answer::Refused(unsupported(&one.name))) else {
        panic!("an older daemon is unchecked");
    };
    assert_eq!(
        older,
        "Server one needs a newer Ployz. It cannot serve ployz.ingress.doors.v1."
    );

    let silent = RpcError::unanswered_target(RpcErrorCode::Unavailable, "no reply".into());
    let Err(cause) = checked(&one, Answer::Refused(silent)) else {
        panic!("a silent Server is unchecked");
    };
    assert!(cause.starts_with("Server one isn't answering"), "{cause}");

    let Err(cause) = checked(&one, Answer::Cancelled) else {
        panic!("a cancelled Deploy leaves it unchecked");
    };
    assert!(cause.contains("cancelled"), "{cause}");
}

// Catches `server set` taking the role back for a failure that isn't the role's, or
// keeping it when the Server's ports are taken.
#[test]
#[expect(clippy::indexing_slicing, reason = "A fixed fixture.")]
fn a_failed_follow_names_each_server_and_how_its_start_failed() {
    let machines = vec![door('a', "one"), door('b', "two"), door('c', "three")];
    let answers = vec![
        answered(stopped_at(
            IngressStage::Holder,
            error(RpcErrorCode::Internal, PORT_TAKEN),
        )),
        answered(both(DoorChange::Unchanged, DoorChange::Unchanged)),
        answered(stopped_at(
            IngressStage::Holder,
            error(RpcErrorCode::InvalidArgument, "invalid mount config"),
        )),
    ];

    let failure = followed(machines.clone(), answers).unwrap_err();

    let diagnostic = failure.diagnostic().expect("a failed follow is diagnosed");
    assert_eq!(diagnostic.failed_machine, Some(machines[0].id));
    assert_eq!(diagnostic.start_failure, Some(StartFailure::PortTaken));
    assert_eq!(
        diagnostic
            .failures
            .iter()
            .map(|detail| detail.message.as_str())
            .collect::<Vec<_>>(),
        [
            "The Ingress Proxy on one isn't running.",
            "The Ingress Proxy on three isn't running."
        ]
    );

    let refused = followed(
        vec![machines[2].clone()],
        vec![answered(stopped_at(
            IngressStage::Holder,
            error(RpcErrorCode::InvalidArgument, "invalid mount config"),
        ))],
    )
    .unwrap_err();
    assert_eq!(refused.diagnostic().unwrap().start_failure, None);

    let crashing = followed(
        vec![machines[1].clone()],
        vec![answered(IngressDoorsSet {
            holder: Some(running(DoorChange::Created)),
            proxy: Some(outcome(
                ContainerRuntimeObservation::Restarting,
                DoorChange::Created,
            )),
            ..IngressDoorsSet::default()
        })],
    )
    .unwrap_err();
    assert_eq!(
        crashing.diagnostic().unwrap().start_failure,
        Some(StartFailure::Unhealthy)
    );

    let fine = followed(
        vec![machines[1].clone()],
        vec![answered(both(DoorChange::Created, DoorChange::Created))],
    )
    .unwrap();
    assert_eq!(fine.len(), 1);
    assert_eq!(fine[0].server.to_string(), "two");
}
