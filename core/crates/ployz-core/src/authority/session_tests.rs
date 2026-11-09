use super::*;
use crate::{
    ContainerId, ContainerKind, OperationId, ServiceName,
    authority::{
        Approver, RestartApproval, RestartFacts, RestartStep, Signed, Signer, SignerKey,
        WorkflowKind,
        restart::{self, Approved},
        trusted_key::tests::{Answer, Authenticator, HONEST, trusting},
    },
};

const ISSUED: u64 = 1_000_000;
const MINUTE: u64 = 60_000;

fn machine(tag: char) -> MachineId {
    MachineId::parse(tag.to_string().repeat(32)).unwrap()
}

fn production() -> Namespace {
    Namespace::parse("production").unwrap()
}

fn production_id() -> Uuid {
    Uuid::from_u128(0x0f8f_ad5b_d9cb_469f_a165_7086_7728_950e)
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([9; 32])
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn browser() -> Signer {
    Signer::from_seed(&[7; 32])
}

fn grant_body(lifetime: u64) -> GrantBody {
    GrantBody {
        browser_key: browser().public(),
        cluster: cluster(),
        environment: production(),
        environment_id: production_id(),
        machines: vec![machine('1'), machine('2')],
        families: vec![ActionFamily::Restart, ActionFamily::Stop],
        issued_at: at(ISSUED),
        expires_at: at(ISSUED + lifetime),
        delegation: Delegation::None,
    }
}

/// The passkey's answer to a session prompt for `body`, as the browser sends it.
fn granted(passkey: &Authenticator, body: &GrantBody, kind: &str) -> SessionGrant {
    let body = canonical::to_canonical(body).unwrap();
    let challenge = GrantBody::challenge(&body);
    let assertion = passkey.answer(&Answer {
        kind,
        challenge: &challenge,
        ..HONEST
    });
    SessionGrant { body, assertion }
}

fn approval(namespace: Namespace, machines: Vec<MachineId>) -> RestartApproval {
    RestartApproval {
        cluster: cluster(),
        operation: OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap(),
        kind: WorkflowKind::Restart,
        namespace,
        service: ServiceName::parse("web").unwrap(),
        machines,
        container: ContainerId::parse("a".repeat(64)).unwrap(),
        issued_at: at(ISSUED + MINUTE),
        expires_at: at(ISSUED + 16 * MINUTE),
        signer: SignerKey::Browser {
            browser: browser().public(),
        },
        proposal: None,
    }
}

fn envelope(approval: &RestartApproval, grant: SessionGrant) -> Signed<RestartApproval> {
    Signed::sign_under(approval, &browser(), grant).unwrap()
}

fn check(
    signed: &Signed<RestartApproval>,
    trusted: &TrustedKeys,
    namespace: &Namespace,
    now: u64,
) -> Result<Approved, AuthorityRefusal> {
    let own = machine('2');
    let service = ServiceName::parse("web").unwrap();
    let container = ContainerId::parse("a".repeat(64)).unwrap();
    restart::verify(
        signed,
        &RestartFacts {
            cluster: Some(cluster()),
            machine: &own,
            trusted,
            namespace,
            service: &service,
            container: &container,
            kind: ContainerKind::ServiceContainer,
            step: RestartStep::Stop,
            now: at(now),
        },
    )
}

#[test]
fn a_browser_key_under_a_trusted_passkeys_grant_approves_a_restart_it_covers() {
    let passkey = Authenticator::es256(4);
    let grant = granted(&passkey, &grant_body(60 * MINUTE), "webauthn.get");
    let signed = envelope(&approval(production(), vec![machine('2')]), grant);
    let approved = check(
        &signed,
        &trusting(&passkey),
        &production(),
        ISSUED + 2 * MINUTE,
    )
    .unwrap();
    assert_eq!(
        approved.by,
        Approver::Browser {
            key: browser().public(),
            grant: Grant {
                body: grant_body(60 * MINUTE),
                passkey: passkey.public_key().code(),
            },
        }
    );
    assert_eq!(
        approved.by.to_string(),
        format!(
            "passkey {} via browser key {}",
            passkey.public_key().code(),
            browser().public().code()
        )
    );
}

#[test]
fn a_grant_whose_expiry_moved_after_the_passkey_answered_fails_its_challenge() {
    let passkey = Authenticator::eddsa([5; 32]);
    let mut grant = granted(&passkey, &grant_body(30 * MINUTE), "webauthn.get");
    grant.body = canonical::to_canonical(&grant_body(60 * MINUTE)).unwrap();
    let signed = envelope(&approval(production(), vec![machine('2')]), grant);
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "a challenge"
        })
    );
}

#[test]
fn a_grant_whose_scope_widened_after_the_passkey_answered_fails_its_challenge() {
    let passkey = Authenticator::es256(4);
    let narrow = GrantBody {
        machines: vec![machine('1')],
        ..grant_body(60 * MINUTE)
    };
    let mut grant = granted(&passkey, &narrow, "webauthn.get");
    grant.body = canonical::to_canonical(&grant_body(60 * MINUTE)).unwrap();
    let signed = envelope(&approval(production(), vec![machine('2')]), grant);
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "a challenge"
        })
    );
}

#[test]
fn a_grant_whose_environment_id_changed_after_the_passkey_answered_fails_its_challenge() {
    let passkey = Authenticator::es256(4);
    let mut grant = granted(&passkey, &grant_body(60 * MINUTE), "webauthn.get");
    grant.body = canonical::to_canonical(&GrantBody {
        environment_id: Uuid::from_u128(1),
        ..grant_body(60 * MINUTE)
    })
    .unwrap();
    let signed = envelope(&approval(production(), vec![machine('2')]), grant);
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "a challenge"
        })
    );
}

#[test]
fn a_grant_without_an_environment_id_lends_its_browser_key_nothing() {
    let passkey = Authenticator::es256(4);
    let mut older = serde_json::to_value(grant_body(60 * MINUTE)).unwrap();
    older.as_object_mut().unwrap().remove("environment_id");
    let body = canonical::to_canonical(&older).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &GrantBody::challenge(&body),
        ..HONEST
    });
    let signed = envelope(
        &approval(production(), vec![machine('2')]),
        SessionGrant { body, assertion },
    );
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::UnknownKey {
            key: browser().public()
        }),
        "a grant that doesn't parse lends its browser key nothing"
    );
}

#[test]
fn a_grant_longer_than_an_hour_is_refused() {
    let passkey = Authenticator::es256(4);
    let grant = granted(&passkey, &grant_body(61 * MINUTE), "webauthn.get");
    let signed = envelope(&approval(production(), vec![machine('2')]), grant);
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "a lifetime"
        })
    );
}

#[test]
fn a_grant_past_its_hour_is_expired() {
    let passkey = Authenticator::es256(4);
    let grant = granted(&passkey, &grant_body(60 * MINUTE), "webauthn.get");
    let mut late = approval(production(), vec![machine('2')]);
    late.issued_at = at(ISSUED + 50 * MINUTE);
    late.expires_at = at(ISSUED + 60 * MINUTE);
    let signed = envelope(&late, grant);
    let after = ISSUED + 60 * MINUTE + validity::CLOCK_SKEW_MILLIS + 1;
    assert_eq!(
        check(&signed, &trusting(&passkey), &production(), after),
        Err(AuthorityRefusal::Expired)
    );
}

#[test]
fn a_registration_answer_never_grants_a_session() {
    let passkey = Authenticator::es256(4);
    let grant = granted(&passkey, &grant_body(60 * MINUTE), "webauthn.create");
    let signed = envelope(&approval(production(), vec![machine('2')]), grant);
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "an assertion type"
        })
    );
}

#[test]
fn a_grant_for_production_does_not_cover_another_environment() {
    let passkey = Authenticator::es256(4);
    let grant = granted(&passkey, &grant_body(60 * MINUTE), "webauthn.get");
    let fix_api = Namespace::parse("fix-api").unwrap();
    let signed = envelope(&approval(fix_api.clone(), vec![machine('2')]), grant);
    assert_eq!(
        check(&signed, &trusting(&passkey), &fix_api, ISSUED + 2 * MINUTE),
        Err(AuthorityRefusal::Mismatch {
            field: "an Environment"
        })
    );
}

#[test]
fn a_grant_does_not_cover_a_machine_it_does_not_name() {
    let passkey = Authenticator::es256(4);
    let grant = granted(&passkey, &grant_body(60 * MINUTE), "webauthn.get");
    let signed = envelope(
        &approval(production(), vec![machine('2'), machine('3')]),
        grant,
    );
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch { field: "Machines" })
    );
}

#[test]
fn a_grant_covers_only_its_families_and_its_window() {
    let passkey = Authenticator::es256(4);
    let stop_only = GrantBody {
        families: vec![ActionFamily::Stop],
        ..grant_body(60 * MINUTE)
    };
    let signed = envelope(
        &approval(production(), vec![machine('2')]),
        granted(&passkey, &stop_only, "webauthn.get"),
    );
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch { field: "an action" })
    );

    let short = grant_body(10 * MINUTE);
    let signed = envelope(
        &approval(production(), vec![machine('2')]),
        granted(&passkey, &short, "webauthn.get"),
    );
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::Mismatch { field: "a window" }),
        "an approval may not outlive its grant"
    );
}

#[test]
fn a_passkey_this_machine_never_approved_grants_nothing() {
    let trusted = Authenticator::es256(4);
    let stranger = Authenticator::es256(6);
    let grant = granted(&stranger, &grant_body(60 * MINUTE), "webauthn.get");
    let signed = envelope(&approval(production(), vec![machine('2')]), grant);
    assert_eq!(
        check(
            &signed,
            &trusting(&trusted),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::UnknownPasskey)
    );
}

#[test]
fn a_browser_key_without_a_grant_naming_it_is_unknown() {
    let passkey = Authenticator::es256(4);
    let other = GrantBody {
        browser_key: Signer::from_seed(&[8; 32]).public(),
        ..grant_body(60 * MINUTE)
    };
    let signed = envelope(
        &approval(production(), vec![machine('2')]),
        granted(&passkey, &other, "webauthn.get"),
    );
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::UnknownKey {
            key: browser().public()
        })
    );
}

#[test]
fn a_body_changed_after_the_browser_key_signed_fails_its_signature() {
    let passkey = Authenticator::es256(4);
    let grant = granted(&passkey, &grant_body(60 * MINUTE), "webauthn.get");
    let mut signed = envelope(&approval(production(), vec![machine('2')]), grant);
    signed.body = signed.body.replace(
        "0f8fad5bd9cb469fa16570867728950e",
        "1f8fad5bd9cb469fa16570867728950e",
    );
    assert_eq!(
        check(
            &signed,
            &trusting(&passkey),
            &production(),
            ISSUED + 2 * MINUTE
        ),
        Err(AuthorityRefusal::BadSignature {
            key: browser().public()
        })
    );
}

#[test]
fn a_device_envelope_keeps_the_bytes_an_older_daemon_reads() {
    let device = Signer::from_seed(&[1; 32]);
    let body = RestartApproval {
        signer: SignerKey::Device(device.public().into()),
        ..approval(production(), vec![machine('2')])
    };
    let signed = Signed::sign(&body, &device).unwrap();
    let json = serde_json::to_value(&signed).unwrap();
    assert!(json.get("grants").is_none(), "{json}");
    assert!(
        signed
            .body
            .contains(&format!(r#""signer":"{}""#, device.public())),
        "{}",
        signed.body
    );
    let browser_body =
        canonical::to_canonical(&approval(production(), vec![machine('2')])).unwrap();
    assert!(
        browser_body.contains(&format!(
            r#""signer":{{"browser":"{}"}}"#,
            browser().public()
        )),
        "{browser_body}"
    );
}

#[test]
fn grant_lists_must_be_sorted_and_distinct() {
    let mut body = grant_body(60 * MINUTE);
    body.families = vec![ActionFamily::Stop, ActionFamily::Restart];
    let text = canonical::to_canonical(&body).unwrap();
    assert!(canonical::from_canonical::<GrantBody>(&text).is_err());
    let mut body = grant_body(60 * MINUTE);
    body.machines = vec![machine('2'), machine('2')];
    let text = canonical::to_canonical(&body).unwrap();
    assert!(canonical::from_canonical::<GrantBody>(&text).is_err());
}

/// The browser's grant body for the fixtures in the dashboard's `approval-session.test.ts`,
/// which must produce these exact bytes and challenge.
#[test]
fn the_browser_writes_this_grant_body_and_challenge() {
    let body = canonical::to_canonical(&grant_body(60 * MINUTE)).unwrap();
    assert_eq!(
        body,
        format!(
            r#"{{"browser_key":"{}","cluster":"{}","delegation":"none","environment":"production","environment_id":"0f8fad5b-d9cb-469f-a165-70867728950e","expires_at":4600000,"families":["restart","stop"],"issued_at":1000000,"machines":["11111111111111111111111111111111","22222222222222222222222222222222"]}}"#,
            browser().public(),
            cluster()
        )
    );
    let challenge = GrantBody::challenge(&body);
    let hex: String = challenge.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        hex,
        "e02a7c52d5c358134a9af54f1b72df1c0fb5b02a89fefb1be3f260c5fcde15df"
    );
    assert_eq!(browser().public().code().to_string(), "72asyextvn");
}

#[test]
fn a_stop_or_start_runs_only_under_a_grant_whose_families_name_it() {
    let passkey = Authenticator::es256(4);
    let own = machine('2');
    let service = ServiceName::parse("web").unwrap();
    let container = ContainerId::parse("a".repeat(64)).unwrap();
    let trusted = trusting(&passkey);
    let verify = |families: Vec<ActionFamily>, kind: WorkflowKind, step: RestartStep| {
        let body = GrantBody {
            families,
            ..grant_body(60 * MINUTE)
        };
        let signed = envelope(
            &RestartApproval {
                kind,
                ..approval(production(), vec![machine('2')])
            },
            granted(&passkey, &body, "webauthn.get"),
        );
        restart::verify(
            &signed,
            &RestartFacts {
                cluster: Some(cluster()),
                machine: &own,
                trusted: &trusted,
                namespace: &production(),
                service: &service,
                container: &container,
                kind: ContainerKind::ServiceContainer,
                step,
                now: at(ISSUED + 2 * MINUTE),
            },
        )
        .map(|approved| approved.body.kind)
    };
    let session = vec![
        ActionFamily::Restart,
        ActionFamily::Start,
        ActionFamily::Stop,
    ];
    for (kind, step) in [
        (WorkflowKind::Restart, RestartStep::Start),
        (WorkflowKind::Stop, RestartStep::Stop),
        (WorkflowKind::Start, RestartStep::Start),
    ] {
        assert_eq!(verify(session.clone(), kind, step), Ok(kind), "{kind}");
    }
    for (kind, step) in [
        (WorkflowKind::Stop, RestartStep::Stop),
        (WorkflowKind::Start, RestartStep::Start),
    ] {
        assert_eq!(
            verify(vec![ActionFamily::Restart], kind, step),
            Err(AuthorityRefusal::Mismatch { field: "an action" }),
            "{kind} under a restart-only grant"
        );
    }
    assert_eq!(
        verify(session, WorkflowKind::Start, RestartStep::Stop),
        Err(AuthorityRefusal::Mismatch {
            field: "a workflow"
        }),
        "a start approval never stops"
    );
}
