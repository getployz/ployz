use super::*;
use crate::{
    DeviceKey,
    authority::{
        keys::Signer,
        restart::OwnPrompt,
        session::{ActionFamily, Delegation, GrantBody, SessionGrant},
        trusted_key::tests::{Answer, Authenticator, HONEST},
        trusted_key::{PasskeyApprover, TrustedPasskey},
    },
};

fn device() -> Signer {
    Signer::from_seed(&[7; 32])
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([9; 32])
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn machine(digit: char) -> MachineId {
    MachineId::parse(digit.to_string().repeat(32)).unwrap()
}

fn namespace(name: &str) -> Namespace {
    Namespace::parse(name).unwrap()
}

fn volume(name: &str) -> DockerVolumeName {
    DockerVolumeName::parse(name).unwrap()
}

fn trusted(passkey: &Authenticator) -> TrustedKeys {
    TrustedKeys {
        devices: vec![DeviceKey::from(device().public())],
        passkeys: vec![TrustedPasskey {
            passkey: passkey.passkey(),
            approved_by: PasskeyApprover::Server,
        }],
    }
}

fn shop() -> TeardownScope {
    TeardownScope::Namespace {
        namespaces: vec![namespace("shop")],
        machines: vec![machine('a'), machine('b')],
        volumes: vec![volume("shop_data")],
    }
}

fn web_3() -> TeardownScope {
    TeardownScope::Server {
        leaving: machine('c'),
        forgetting: vec![machine('a'), machine('b')],
        volumes: vec![DockerVolumeName::parse("shop_data").unwrap()],
    }
}

fn teardown(scope: TeardownScope, signer: SignerKey) -> Teardown {
    Teardown::drafted(TeardownTerms {
        cluster: cluster(),
        scope,
        issued_at: at(1_000),
        signer,
    })
}

fn by_device(scope: TeardownScope) -> TeardownApproval {
    TeardownApproval::Signed(
        Signed::sign(
            &teardown(scope, SignerKey::Device(device().public().into())),
            &device(),
        )
        .unwrap(),
    )
}

fn by_passkey(passkey: &Authenticator, scope: TeardownScope) -> TeardownApproval {
    let body = Signed::unsigned(&teardown(scope, SignerKey::Passkey(OwnPrompt::Passkey)))
        .unwrap()
        .body;
    let assertion = passkey.answer(&Answer {
        challenge: &key_approval_challenge(&body),
        ..HONEST
    });
    TeardownApproval::Passkey { body, assertion }
}

fn check(
    approval: &TeardownApproval,
    trusted: &TrustedKeys,
    on: char,
    step: TeardownStep<'_>,
) -> Result<Approver, AuthorityRefusal> {
    verify(
        approval,
        &TeardownFacts {
            cluster: Some(cluster()),
            machine: &machine(on),
            trusted,
            step,
            now: at(2_000),
        },
    )
    .map(|approved| approved.by)
}

fn remove_in(namespace: &Namespace) -> TeardownStep<'_> {
    TeardownStep::Container {
        namespace,
        step: RestartStep::Remove,
    }
}

#[test]
fn a_device_or_a_passkeys_own_prompt_takes_the_namespace_off_the_servers_it_names() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let shop = namespace("shop");
    for step in [
        RestartStep::Hold,
        RestartStep::Out,
        RestartStep::Stop,
        RestartStep::Remove,
    ] {
        let step = TeardownStep::Container {
            namespace: &shop,
            step,
        };
        assert_eq!(
            check(&by_device(self::shop()), &trusted, 'b', step),
            Ok(Approver::Device(device().public()))
        );
        assert_eq!(
            check(&by_passkey(&passkey, self::shop()), &trusted, 'a', step),
            Ok(Approver::Passkey(passkey.passkey().code()))
        );
    }
    let data = volume("shop_data");
    assert!(
        check(
            &by_device(self::shop()),
            &trusted,
            'a',
            TeardownStep::Volume(&data)
        )
        .is_ok()
    );
}

#[test]
fn a_teardown_never_starts_a_container() {
    let passkey = Authenticator::es256(4);
    let shop = namespace("shop");
    assert_eq!(
        check(
            &by_device(self::shop()),
            &trusted(&passkey),
            'a',
            TeardownStep::Container {
                namespace: &shop,
                step: RestartStep::Start,
            },
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "a Namespace on this Server"
        })
    );
}

#[test]
fn another_namespace_machine_or_volume_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let approval = by_device(shop());
    assert_eq!(
        check(&approval, &trusted, 'a', remove_in(&namespace("staging"))),
        Err(AuthorityRefusal::Mismatch {
            field: "a Namespace on this Server"
        })
    );
    assert_eq!(
        check(&approval, &trusted, 'c', remove_in(&namespace("shop"))),
        Err(AuthorityRefusal::Mismatch {
            field: "a Namespace on this Server"
        })
    );
    let data = volume("shop_data");
    assert!(check(&approval, &trusted, 'b', TeardownStep::Volume(&data)).is_ok());
    assert_eq!(
        check(&approval, &trusted, 'c', TeardownStep::Volume(&data)),
        Err(AuthorityRefusal::Mismatch {
            field: "a Volume on this Server"
        }),
        "a Volume goes only on the Servers named"
    );
    let other = volume("shop_cache");
    assert!(check(&approval, &trusted, 'a', TeardownStep::Volume(&other)).is_err());
}

#[test]
fn a_volume_outside_the_named_namespaces_spoils_the_whole_teardown() {
    let passkey = Authenticator::es256(4);
    let approval = by_device(TeardownScope::Namespace {
        namespaces: vec![namespace("shop")],
        machines: vec![machine('a')],
        volumes: vec![volume("staging_data")],
    });
    assert_eq!(
        check(
            &approval,
            &trusted(&passkey),
            'a',
            remove_in(&namespace("shop"))
        ),
        Err(AuthorityRefusal::Mismatch { field: "a Volume" })
    );
}

#[test]
fn a_deploys_volume_teardown_takes_no_container_off() {
    let passkey = Authenticator::es256(4);
    let approval = by_device(TeardownScope::Volumes {
        machines: vec![machine('a')],
        volumes: vec![volume("shop_data")],
    });
    let data = volume("shop_data");
    assert!(
        check(
            &approval,
            &trusted(&passkey),
            'a',
            TeardownStep::Volume(&data)
        )
        .is_ok()
    );
    assert!(
        check(
            &approval,
            &trusted(&passkey),
            'a',
            remove_in(&namespace("shop"))
        )
        .is_err()
    );
    assert!(
        check(
            &approval,
            &trusted(&passkey),
            'b',
            TeardownStep::Volume(&data)
        )
        .is_err()
    );
}

#[test]
fn a_server_teardown_lets_only_the_named_server_leave_and_the_others_forget_it() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let approval = by_device(web_3());
    assert!(check(&approval, &trusted, 'c', TeardownStep::Leave).is_ok());
    assert_eq!(
        check(&approval, &trusted, 'b', TeardownStep::Leave),
        Err(AuthorityRefusal::Mismatch {
            field: "this Server"
        })
    );
    assert!(
        check(
            &approval,
            &trusted,
            'a',
            TeardownStep::Forget(&machine('c'))
        )
        .is_ok()
    );
    assert_eq!(
        check(
            &approval,
            &trusted,
            'a',
            TeardownStep::Forget(&machine('b'))
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "a Server to forget"
        })
    );
    assert!(
        check(
            &approval,
            &trusted,
            'd',
            TeardownStep::Forget(&machine('c'))
        )
        .is_err(),
        "a Server the teardown doesn't name forgets nothing"
    );
    assert!(check(&approval, &trusted, 'c', remove_in(&namespace("shop"))).is_err());
}

#[test]
fn another_cluster_or_an_expired_teardown_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let shop = namespace("shop");
    let elsewhere = verify(
        &by_device(self::shop()),
        &TeardownFacts {
            cluster: Some(SealingPublicKey::from_bytes([8; 32])),
            machine: &machine('a'),
            trusted: &trusted,
            step: remove_in(&shop),
            now: at(2_000),
        },
    );
    assert_eq!(
        elsewhere.map(|approved| approved.by),
        Err(AuthorityRefusal::Mismatch { field: "a Cluster" })
    );
    let late = verify(
        &by_device(self::shop()),
        &TeardownFacts {
            cluster: Some(cluster()),
            machine: &machine('a'),
            trusted: &trusted,
            step: remove_in(&shop),
            now: at(10_000_000),
        },
    );
    assert_eq!(
        late.map(|approved| approved.by),
        Err(AuthorityRefusal::Expired)
    );
}

#[test]
fn a_browser_key_under_a_session_grant_is_refused() {
    let passkey = Authenticator::es256(4);
    let browser = Signer::from_seed(&[6; 32]);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: cluster(),
        environment: namespace("shop"),
        environment_id: uuid::Uuid::from_u128(1),
        machines: vec![machine('a'), machine('b')],
        families: vec![ActionFamily::Deploy],
        issued_at: at(0),
        expires_at: at(3_000_000),
        delegation: Delegation::None,
    };
    let unsigned = Signed::unsigned(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &unsigned.challenge(),
        ..HONEST
    });
    let signed = Signed::sign_under(
        &teardown(
            shop(),
            SignerKey::Browser {
                browser: browser.public(),
            },
        ),
        &browser,
        SessionGrant {
            body: unsigned.body.clone(),
            assertion,
        },
    )
    .unwrap();
    assert_eq!(
        check(
            &TeardownApproval::Signed(signed),
            &trusted(&passkey),
            'a',
            remove_in(&namespace("shop"))
        ),
        Err(AuthorityRefusal::Mismatch {
            field: "the signer"
        })
    );
}

#[test]
fn a_passkey_answer_over_another_body_or_inside_an_envelope_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let shop = namespace("shop");
    let TeardownApproval::Passkey { assertion, .. } = by_passkey(&passkey, self::shop()) else {
        unreachable!()
    };
    let wider = Signed::unsigned(&teardown(
        TeardownScope::Namespace {
            namespaces: vec![namespace("shop"), namespace("staging")],
            machines: vec![machine('a'), machine('b')],
            volumes: Vec::new(),
        },
        SignerKey::Passkey(OwnPrompt::Passkey),
    ))
    .unwrap()
    .body;
    assert!(
        check(
            &TeardownApproval::Passkey {
                body: wider,
                assertion
            },
            &trusted,
            'a',
            remove_in(&shop)
        )
        .is_err()
    );
    let unsigned = Signed::unsigned(&teardown(
        self::shop(),
        SignerKey::Passkey(OwnPrompt::Passkey),
    ))
    .unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &unsigned.challenge(),
        ..HONEST
    });
    assert!(
        check(
            &TeardownApproval::Signed(unsigned.answered(assertion)),
            &trusted,
            'a',
            remove_in(&shop)
        )
        .is_err()
    );
}

#[test]
fn an_unknown_device_is_refused() {
    let passkey = Authenticator::es256(4);
    let stranger = Signer::from_seed(&[8; 32]);
    let signed = Signed::sign(
        &teardown(shop(), SignerKey::Device(stranger.public().into())),
        &stranger,
    )
    .unwrap();
    assert_eq!(
        check(
            &TeardownApproval::Signed(signed),
            &trusted(&passkey),
            'a',
            remove_in(&namespace("shop"))
        ),
        Err(AuthorityRefusal::UnknownKey {
            key: stranger.public()
        })
    );
}

#[test]
fn each_scope_signs_its_golden_canonical_body() {
    let (a, b, c) = ("a".repeat(32), "b".repeat(32), "c".repeat(32));
    let head = r#"{"cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","expires_at":901000,"issued_at":1000,"scope":"#;
    let golden = [
        (
            shop(),
            format!(
                r#"{{"kind":"namespace","machines":["{a}","{b}"],"namespaces":["shop"],"volumes":["shop_data"]}}"#
            ),
        ),
        (
            TeardownScope::Volumes {
                machines: vec![machine('a')],
                volumes: vec![volume("shop_data")],
            },
            format!(r#"{{"kind":"volumes","machines":["{a}"],"volumes":["shop_data"]}}"#),
        ),
        (
            web_3(),
            format!(
                r#"{{"forgetting":["{a}","{b}"],"kind":"server","leaving":"{c}","volumes":["shop_data"]}}"#
            ),
        ),
    ];
    for (scope, golden) in golden {
        let body = Signed::unsigned(&teardown(scope, SignerKey::Passkey(OwnPrompt::Passkey)))
            .unwrap()
            .body;
        assert_eq!(body, format!(r#"{head}{golden},"signer":"passkey"}}"#));
    }
}

#[test]
fn a_draft_sorts_its_lists_and_the_lines_name_every_loss() {
    let drafted = teardown(
        TeardownScope::Namespace {
            namespaces: vec![namespace("shop"), namespace("shop")],
            machines: vec![machine('b'), machine('a')],
            volumes: vec![volume("shop_data")],
        },
        SignerKey::Passkey(OwnPrompt::Passkey),
    );
    assert_eq!(drafted.scope, shop());
    let name = |machine: &MachineId| format!("web-{}", &machine.to_string()[..1]);
    let label = |volume: &DockerVolumeName| match volume.as_str() {
        "shop_data" => "data".to_owned(),
        other => other.to_owned(),
    };
    assert_eq!(
        drafted.lines(name, label),
        [
            "Approved: stop and remove every service of shop on web-a, web-b.",
            "Approved: delete volume data (shop_data) there. Its data is gone for good.",
        ]
    );
    assert_eq!(
        teardown(
            TeardownScope::Volumes {
                machines: vec![machine('b'), machine('a')],
                volumes: vec![volume("shop_data")],
            },
            SignerKey::Passkey(OwnPrompt::Passkey),
        )
        .lines(name, label),
        ["Approved: delete volume data (shop_data) on web-a, web-b. Its data is gone for good."]
    );
    assert_eq!(
        teardown(web_3(), SignerKey::Passkey(OwnPrompt::Passkey)).lines(name, label),
        [
            "Approved: remove web-c from the Cluster. Its services stop there.",
            "Approved: web-a, web-b forget web-c.",
            "The Cluster can no longer reach its copies of data (shop_data).",
        ]
    );
    let relayed = |_: &MachineId| "web\u{1b}[2J".to_owned();
    assert!(
        teardown(web_3(), SignerKey::Passkey(OwnPrompt::Passkey))
            .lines(relayed, label)
            .iter()
            .all(|line| !line.contains('\u{1b}')),
        "a relayed name never reaches the terminal raw"
    );
}

#[test]
fn the_device_the_body_names_approves_whatever_order_its_signature_comes_in() {
    let passkey = Authenticator::es256(4);
    let other = Signer::from_seed(&[8; 32]);
    let mut trusted = trusted(&passkey);
    trusted.devices.push(DeviceKey::from(other.public()));
    let TeardownApproval::Signed(mut signed) = by_device(shop()) else {
        unreachable!()
    };
    let theirs = Signed::<Teardown>::sign(&signed.open().unwrap(), &other).unwrap();
    signed.signatures.splice(0..0, theirs.signatures);
    assert_eq!(
        check(
            &TeardownApproval::Signed(signed),
            &trusted,
            'a',
            remove_in(&namespace("shop"))
        ),
        Ok(Approver::Device(device().public()))
    );
}
