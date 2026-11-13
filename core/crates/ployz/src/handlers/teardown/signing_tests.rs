use ployz_core::authority::{OwnPrompt, Signed};
use ployz_core::{DockerVolumeName, Namespace, SealingPublicKey};

use super::*;

fn machine(digit: char) -> MachineId {
    MachineId::parse(digit.to_string().repeat(32)).unwrap()
}

fn drafted() -> Drafted {
    let signer = Signer::from_seed(&[7; 32]);
    let terms = TeardownTerms {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        scope: TeardownScope::Namespace {
            namespaces: vec![Namespace::parse("shop-production").unwrap()],
            machines: vec![machine('b'), machine('a')],
            volumes: vec![DockerVolumeName::parse("shop-production_vol-1").unwrap()],
        },
        issued_at: Timestamp::from_millis(1_000).unwrap(),
        signer: SignerKey::Device(signer.public().into()),
    };
    let lines = Teardown::drafted(terms.clone()).lines(
        |id| format!("web-{}", &id.to_string()[..1]),
        |_| "data".to_owned(),
    );
    Drafted {
        terms,
        signer,
        lines,
    }
}

/// The lines a person reads are the lines of the body this Device signs, the Volume whose
/// data goes among them.
#[test]
fn the_signed_body_reads_as_the_lines_shown() {
    let drafted = drafted();
    let shown = drafted.lines().to_vec();
    assert_eq!(
        shown,
        [
            "Approved: stop and remove every service of shop-production on web-a, web-b.",
            "Approved: delete volume data (shop-production_vol-1) there. Its data is gone for good.",
        ]
    );
    let TeardownApproval::Signed(signed) = drafted.sign().unwrap() else {
        panic!("a Device signs in the envelope");
    };
    let body = signed.open().unwrap();
    assert_eq!(
        body.lines(
            |id| format!("web-{}", &id.to_string()[..1]),
            |_| "data".to_owned()
        ),
        shown
    );
    assert!(body.issued_at.as_millis() > 1_000, "stamped when signed");
    assert!(!matches!(
        body.signer,
        SignerKey::Passkey(OwnPrompt::Passkey)
    ));
    let _ = Signed::<Teardown>::unsigned(&body).unwrap();
}

#[test]
fn yes_signs_without_asking() {
    consent(true, "Delete it?", |_, _, _| panic!("asked")).unwrap();
}

/// Without `--yes`, a run that can't ask refuses before anything is signed.
#[test]
fn no_terminal_and_no_yes_refuses_before_signing() {
    let refused = consent(false, "Delete it?", |question, refusal, _| {
        assert_eq!(question, "Delete it?");
        Err(refusal)
    })
    .unwrap_err();
    let rpc = refused.report();
    assert_eq!(rpc.code, RpcErrorCode::ConfirmationRequired);
    assert!(rpc.message.contains("add --yes"), "{}", rpc.message);
}
